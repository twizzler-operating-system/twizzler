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
    Request,
    request::{ReqKind, RequestMapAdapter},
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

    /// Build the wire requests for this inflight entry.
    ///
    /// `required` is the page range the submitting thread is actually blocked on, in absolute
    /// object pages. Like the requester flags below it is applied *here* rather than carried in the
    /// [ReqKind], because [ReqKind] is the coalescing key and this varies per requesting thread --
    /// putting it in the key would leave two entries in flight for one range. This runs on the
    /// thread that is about to wait, and only for the request that actually sends, so a coalescing
    /// waiter inherits the first submitter's required range. That is a latency choice, not a
    /// correctness one: it only decides which pages the pager hurries.
    pub(super) fn for_each_pager_req(
        &self,
        required: Option<(usize, usize)>,
        mut f: impl FnMut(RequestFromKernel),
    ) {
        if !self.needs_send {
            return;
        }
        let required = required
            .map(|(start, len)| {
                ObjectRange::new(
                    (start * NULLPAGE_SIZE) as u64,
                    ((start + len) * NULLPAGE_SIZE) as u64,
                )
            })
            .unwrap_or(ObjectRange::new(0, 0));
        let cmd = match &self.rk {
            ReqKind::Info(obj_id) => KernelCommand::ObjectInfoReq(*obj_id),
            // The requester tag is added here rather than in the `ReqKind` because `ReqKind` is the
            // coalescing key -- `add_request` finds an existing request by it, using the *derived*
            // `Ord`, which compares the flags. A flag that varies by requesting thread would put
            // two entries in flight for one range, which is the shape suspected of wedging the
            // guest when prefetch last set a flag (see the removal note in `pager.rs`). This runs
            // on the thread that is about to wait, and only for the request that actually sends, so
            // a coalescing waiter simply inherits the first submitter's tag.
            ReqKind::PageData(obj_id, s, l, f) => KernelCommand::PageDataReq(
                *obj_id,
                ObjectRange::new((s * NULLPAGE_SIZE) as u64, ((s + l) * NULLPAGE_SIZE) as u64),
                *f | crate::pager::requester_flags(),
                required,
            ),
            ReqKind::Sync(obj_id) => KernelCommand::ObjectEvict(ObjectEvictInfo {
                obj_id: *obj_id,
                range: ObjectRange::new(0, 0),
                phys: PhysRange::new(0, 0),
                version: 0,
                flags: ObjectEvictFlags::SYNC | ObjectEvictFlags::FENCE,
                uniq_id: 0.into(),
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

    pub fn check_timed_out_requests(&self) {
        for req in self.req_map.iter() {
            if req.is_timed_out() {
                log::warn!("request timed out: {:?}", req.reqkind());
            }
        }
    }

    pub fn add_request(&mut self, rk: ReqKind) -> Result<Inflight, ReqKind> {
        if let Some(req) = self.req_map.find(&rk).get() {
            log::trace!(
                "found existing request {:?} for request {:?}",
                req.reqkind(),
                rk
            );
            return Ok(Inflight::new(req.id, rk, false));
        }

        // A demand fault whose range is already being prefetched waits on that request rather than
        // issuing a second one for the same pages. Returning the *prefetch's* key is what makes the
        // rest work unchanged: `setup_wait` compares against it, and the completion the pager sends
        // removes and signals under it.
        //
        // Worst case is a prefetch the pager declines over `MAX_INFLIGHT_PREFETCH`, which is acked
        // DONE with no pages: the waiter wakes, finds its pages absent, and the fault retries and
        // issues its own request. One extra fault, not a stall.
        if let Some(twin) = rk.prefetch_twin() {
            if let Some(req) = self.req_map.find(&twin).get() {
                log::trace!(
                    "demand request {:?} coalescing onto prefetch {:?}",
                    rk,
                    twin
                );
                return Ok(Inflight::new(req.id, twin, false));
            }
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
            return Err(rk);
        };
        let request = Request::new(id, rk.clone());
        assert!(self.requests[id].is_none());
        self.requests[id] = Some(request);
        let request = self.requests[id].as_ref().unwrap();
        self.req_map
            .insert(unsafe { (request as *const Request).as_ref().unwrap_unchecked() });
        Ok(Inflight::new(id, rk, true))
    }

    pub fn remove_request(&mut self, rk: &ReqKind) {
        if let Some(request) = self.req_map.find_mut(rk).remove() {
            request.mark_done();
            request.signal();
            let id = request.id;
            self.avail.bit_set(id);
            self.requests[id] = None;
        } else {
            // Every completion the pager marks DONE lands here, so a miss means a request that has
            // been answered is still in the map: its waiters will never be signalled and its slot
            // is leaked. This was silent, which is most of why the comparator bug it reports (see
            // `ReqKind`) took two attempts to find.
            log::warn!("completed a pager request that is not in the map: {:?}", rk);
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
        // The slot index does not identify a request on its own. Every caller drops the manager
        // lock between `add_request` and here in order to submit, and in that window the request
        // can complete, be removed, and have its slot handed to something else -- at which point
        // parking on the occupant means waiting for a completion that has nothing to do with us,
        // and being woken (or not) by it. Declining to wait is always safe: every caller re-checks
        // its own condition, and the ones that loop will simply come back round.
        if request.reqkind() != &inflight.rk {
            log::warn!(
                "pager request slot {} was recycled under a waiter: wanted {:?}, found {:?}",
                inflight.id,
                inflight.rk,
                request.reqkind()
            );
            return None;
        }
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

    pub fn with_request<R>(&mut self, rk: &ReqKind, f: impl FnOnce(&Request) -> R) -> Option<R> {
        Some(f(self.req_map.find_mut(rk).get()?))
    }

    pub fn set_ready(&mut self) {
        self.pager_ready = true;
    }

    pub fn is_ready(&self) -> bool {
        self.pager_ready
    }
}
