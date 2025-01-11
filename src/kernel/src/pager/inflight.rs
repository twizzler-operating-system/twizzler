use alloc::collections::{btree_map::BTreeMap, btree_set::BTreeSet};

use stable_vec::StableVec;
use twizzler_abi::{
    object::{ObjID, NULLPAGE_SIZE},
    pager::{KernelCommand, ObjectRange, RequestFromKernel},
};

use super::{request::ReqKind, Request};
use crate::thread::{CriticalGuard, ThreadRef};

pub struct Inflight {
    id: usize,
    rk: ReqKind,
    needs_send: bool,
}

impl Inflight {
    pub(super) fn new(id: usize, rk: ReqKind, needs_send: bool) -> Self {
        Self { id, rk, needs_send }
    }

    pub(super) fn pager_req(&self) -> Option<RequestFromKernel> {
        if !self.needs_send {
            return None;
        }
        let cmd = match self.rk {
            ReqKind::Info(obj_id) => KernelCommand::ObjectInfoReq(obj_id),
            ReqKind::PageData(obj_id, s, e) => KernelCommand::PageDataReq(
                obj_id,
                ObjectRange::new((s * NULLPAGE_SIZE) as u64, (e * NULLPAGE_SIZE) as u64),
            ),
            ReqKind::Sync(obj_id) => KernelCommand::ObjectSync(obj_id),
            ReqKind::Del(obj_id) => todo!(),
            ReqKind::Create(obj_id) => todo!(),
        };
        Some(RequestFromKernel::new(cmd))
    }
}

#[derive(Default)]
struct PerObjectData {
    page_map: BTreeMap<usize, BTreeSet<usize>>,
    info_list: BTreeSet<usize>,
    sync_list: BTreeSet<usize>,
}

impl PerObjectData {
    fn insert(&mut self, rk: ReqKind, id: usize) {
        for page in rk.pages() {
            self.page_map.entry(page).or_default().insert(id);
        }
        if rk.needs_info() {
            self.info_list.insert(id);
        }
        if rk.needs_sync() {
            self.sync_list.insert(id);
        }
    }

    fn remove_all(&mut self, rk: ReqKind, id: usize) {
        for page in rk.pages() {
            self.page_map.entry(page).or_default().remove(&id);
        }
        if rk.needs_info() {
            self.info_list.remove(&id);
        }
        if rk.needs_sync() {
            self.sync_list.remove(&id);
        }
    }
}

pub(super) struct InflightManager {
    requests: StableVec<Request>,
    req_map: BTreeMap<ReqKind, usize>,
    per_object: BTreeMap<ObjID, PerObjectData>,
}

impl InflightManager {
    pub fn new() -> Self {
        Self {
            requests: StableVec::new(),
            req_map: BTreeMap::new(),
            per_object: BTreeMap::new(),
        }
    }

    pub fn add_request(&mut self, rk: ReqKind) -> Inflight {
        if let Some(id) = self.req_map.get(&rk) {
            return Inflight::new(*id, rk, false);
        }
        let id = self.requests.next_push_index();
        let request = Request::new(id, rk);
        self.requests.push(request);
        self.req_map.insert(rk, id);
        let per_obj = self
            .per_object
            .entry(rk.objid())
            .or_insert_with(|| PerObjectData::default());
        per_obj.insert(rk, id);
        Inflight::new(id, rk, true)
    }

    fn remove_request(&mut self, id: usize) {
        let Some(request) = self.requests.get(id) else {
            return;
        };
        self.req_map.remove(&request.reqkind());
        if let Some(po) = self.per_object.get_mut(&request.reqkind().objid()) {
            po.remove_all(request.reqkind(), id);
        }
    }

    pub fn setup_wait<'a>(
        &mut self,
        inflight: &Inflight,
        thread: &'a ThreadRef,
    ) -> Option<CriticalGuard<'a>> {
        let Some(request) = self.requests.get_mut(inflight.id) else {
            return None;
        };
        request.setup_wait(thread)
    }

    pub fn cmd_ready(&mut self, objid: ObjID, sync: bool) {
        if let Some(po) = self.per_object.get_mut(&objid) {
            let list = if sync { &po.sync_list } else { &po.info_list };
            for id in list {
                if let Some(req) = self.requests.get_mut(*id) {
                    req.cmd_ready();
                    if req.done() {
                        req.signal();
                    }
                } else {
                    logln!("[pager] warning -- stale ID");
                }
            }
        }
    }

    pub fn pages_ready(&mut self, objid: ObjID, pages: impl IntoIterator<Item = usize>) {
        if let Some(po) = self.per_object.get_mut(&objid) {
            for page in pages {
                if let Some(idset) = po.page_map.get(&page) {
                    for id in idset {
                        if let Some(req) = self.requests.get_mut(*id) {
                            req.page_ready(page);
                            if req.done() {
                                req.signal();
                            }
                        } else {
                            logln!("[pager] warning -- stale ID");
                        }
                    }
                }
            }
        }
    }
}
