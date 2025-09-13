use core::u64;

use intrusive_collections::RBTree;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    pager::{
        KernelCommand, ObjectEvictFlags, ObjectEvictInfo, ObjectInfo, ObjectRange, PhysRange,
        RequestFromKernel,
    },
    syscall::LifetimeType,
};

use super::{
    request::{ReqKind, RequestMapAdapter},
    Request,
};
use crate::thread::{CriticalGuard, ThreadRef};

pub struct Inflight {
    id: usize,
    pub rk: ReqKind,
    needs_send: bool,
}

impl Inflight {
    pub(super) fn new(id: usize, rk: ReqKind, needs_send: bool) -> Self {
        Self { id, rk, needs_send }
    }

    pub(super) fn for_each_pager_req(&self, mut f: impl FnMut(RequestFromKernel)) {
        if !self.needs_send {
            return;
        }
        let cmd = match &self.rk {
            ReqKind::Info(obj_id) => KernelCommand::ObjectInfoReq(*obj_id),
            ReqKind::PageData(obj_id, s, l, f) => KernelCommand::PageDataReq(
                *obj_id,
                ObjectRange::new((s * NULLPAGE_SIZE) as u64, ((s + l) * NULLPAGE_SIZE) as u64),
                *f,
            ),
            ReqKind::Sync(obj_id) => KernelCommand::ObjectEvict(ObjectEvictInfo {
                obj_id: *obj_id,
                range: ObjectRange::new(0, 0),
                phys: PhysRange::new(0, 0),
                version: 0,
                flags: ObjectEvictFlags::SYNC | ObjectEvictFlags::FENCE,
            }),
            ReqKind::Del(obj_id) => KernelCommand::ObjectDel(*obj_id),
            ReqKind::Create(obj_id, create, nonce) => KernelCommand::ObjectCreate(
                *obj_id,
                ObjectInfo::new(
                    LifetimeType::Persistent,
                    create.bt,
                    create.kuid,
                    *nonce,
                    create.def_prot,
                ),
            ),
            ReqKind::Pages(phys_range) => KernelCommand::DramPages(*phys_range),
            ReqKind::SyncRegion(info) => {
                for e in &**info.reqs {
                    f(*e);
                }
                return;
            }
        };
        f(RequestFromKernel::new(cmd))
    }
}

pub(super) const NR_REQUESTS: usize = 256;
use bitset_core::BitSet;
pub(super) struct InflightManager {
    requests: [Option<Request>; NR_REQUESTS],
    avail: [u64; NR_REQUESTS / 64],
    req_map: RBTree<RequestMapAdapter>,
    pager_ready: bool,
}

impl InflightManager {
    pub fn new() -> Self {
        Self {
            requests: [const { None }; NR_REQUESTS],
            avail: [!0; NR_REQUESTS / 64],
            req_map: RBTree::new(RequestMapAdapter::NEW),
            pager_ready: false,
        }
    }

    pub fn add_request(&mut self, rk: ReqKind) -> Option<Inflight> {
        if let Some(req) = self.req_map.find(&rk).get() {
            return Some(Inflight::new(req.id, rk, false));
        }

        let mut id = None;
        for b in 0..NR_REQUESTS {
            if self.avail.bit_test(b) {
                self.avail.bit_reset(b);
                id = Some(b);
                break;
            }
        }

        let Some(id) = id else {
            return None;
        };
        let request = Request::new(id, rk.clone());
        assert!(self.requests[id].is_none());
        self.requests[id] = Some(request);
        let request = self.requests[id].as_ref().unwrap();
        self.req_map
            .insert(unsafe { (request as *const Request).as_ref().unwrap_unchecked() });
        Some(Inflight::new(id, rk, true))
    }

    pub fn remove_request(&mut self, rk: &ReqKind) {
        if let Some(request) = self.req_map.find_mut(rk).remove() {
            let id = request.id;
            self.avail.bit_set(id);
            self.requests[id] = None;
        }
    }

    pub fn setup_wait<'a>(
        &mut self,
        inflight: &Inflight,
        thread: &'a ThreadRef,
    ) -> Option<CriticalGuard<'a>> {
        let Some(Some(request)) = self.requests.get_mut(inflight.id) else {
            return None;
        };
        request.setup_wait(thread)
    }

    pub fn request_ready(&mut self, rk: &ReqKind) {
        let cursor = self.req_map.find_mut(rk);
        if let Some(request) = cursor.get() {
            request.mark_done();
            request.signal();
        } else {
            log::warn!("failed to find request: {:?}", rk);
        }
    }

    pub fn set_ready(&mut self) {
        self.pager_ready = true;
    }

    pub fn is_ready(&self) -> bool {
        self.pager_ready
    }
}
