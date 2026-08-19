use core::u64;

use intrusive_collections::{Bound, RBTree};
use twizzler_abi::{
    object::{NULLPAGE_SIZE, ObjID},
    pager::{
        KernelCommand, ObjectEvictFlags, ObjectEvictInfo, ObjectInfo, ObjectRange, PagerFlags,
        PhysRange, RequestFromKernel,
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
    /// Requests holding a slot right now. Tracked rather than counted off `req_map`, which is a
    /// tree walk, so that the submit path can report how many were already outstanding.
    live: usize,
}

impl InflightManager {
    /// Whether a page-data request may coalesce onto one covering only the pages its caller blocks
    /// on, rather than one covering its whole widened range. See `page_data_request`.
    const COALESCE_ON_REQUIRED: bool = true;

    pub fn new() -> Self {
        Self {
            requests: [const { None }; NR_REQUESTS],
            avail: [!0; NR_REQUESTS / 64],
            req_map: RBTree::new(RequestMapAdapter::NEW),
            pager_ready: false,
            live: 0,
        }
    }

    /// The page-data requests in flight for `id`, in ascending start order.
    ///
    /// `req_map` is keyed by `ReqKind`, whose derived `Ord` compares the variant before
    /// `(id, start, len, flags)`, so every `PageData` for one object forms a single contiguous run
    /// of the tree sorted by start. That is what makes a range scan possible at all: the overlap
    /// relation cannot order a search tree, which is the point [`ReqKind`]'s comment makes.
    fn page_data_for(&self, id: ObjID) -> impl Iterator<Item = (usize, usize, &Request)> {
        let low = ReqKind::new_page_data(id, 0, 0, PagerFlags::empty());
        let mut cursor = self.req_map.lower_bound(Bound::Included(&low));
        core::iter::from_fn(move || {
            // Anything sorting after this object's page-data run ends the scan: later objects sort
            // higher on `id`, and every other variant sorts after `PageData` outright.
            let out = match cursor.get()?.reqkind() {
                ReqKind::PageData(oid, s, l, _) if *oid == id => (*s, *l, cursor.get()?),
                _ => return None,
            };
            cursor.move_next();
            Some(out)
        })
    }

    /// The page-data ranges in flight for `id`. Diagnostic only.
    pub fn page_data_ranges(&self, id: ObjID) -> heapless::Vec<(usize, usize), 8> {
        let mut v = heapless::Vec::new();
        for (s, l, _) in self.page_data_for(id) {
            if v.push((s, l)).is_err() {
                break;
            }
        }
        v
    }

    /// An in-flight request whose range already covers `[start, start + len)`, if there is one.
    fn covering_page_data(&self, id: ObjID, start: usize, len: usize) -> Option<(usize, ReqKind)> {
        let end = start + len;
        self.page_data_for(id)
            // Ascending by start, so once a request begins after ours none of the rest can cover
            // it.
            .take_while(|(s, _, _)| *s <= start)
            .find(|(s, l, _)| *s + *l >= end)
            .map(|(_, _, req)| (req.id, req.reqkind().clone()))
    }

    /// Narrow `[start, start + len)` against what is already in flight, never giving up a page the
    /// caller is blocked on.
    ///
    /// Only the head and the tail are trimmed, never a hole in the middle: the widening in
    /// `ensure_in_core_pager` extends the caller's range outward, so that is where the overlap with
    /// a request already in flight is, and a range with a hole would have to become two requests.
    ///
    /// The pages given up here are ones another request is already fetching. If that request is
    /// declined -- a prefetch over the pager's cap is acked DONE with no pages -- they simply do
    /// not arrive, and since nothing is blocked on them a later fault asks again. One extra
    /// fault, not a stall, which is the same trade the prefetch coalescing above already makes.
    fn narrow_page_data(
        &self,
        id: ObjID,
        start: usize,
        len: usize,
        required: Option<(usize, usize)>,
    ) -> (usize, usize) {
        let (mut lo, mut hi) = (start, start + len);
        let (rlo, rhi) = match required {
            // Clamped into the range being narrowed: `required` is the caller's pre-widening ask,
            // which a split request need not contain.
            Some((p, l)) if p < hi && p + l > lo => (p.max(lo), (p + l).min(hi)),
            // Nothing here is being waited on, so every page of it may be given up.
            _ => (hi, lo),
        };

        for (s, l, _) in self.page_data_for(id) {
            let (a, b) = (s, s + l);
            if a <= lo && b > lo {
                lo = b.min(rlo);
            }
            if b >= hi && a < hi {
                hi = a.max(rhi);
            }
            if lo >= hi {
                break;
            }
        }
        (lo, hi.max(lo))
    }

    /// Build the page-data request to send for `[start, start + len)`, narrowed against the
    /// requests already in flight. `required` is the range the caller will block on.
    ///
    /// Returns the original range when narrowing empties it: that means the whole thing is already
    /// being fetched, and `add_request` will attach the caller to the covering request rather than
    /// send anything.
    pub fn page_data_request(
        &self,
        id: ObjID,
        start: usize,
        len: usize,
        flags: PagerFlags,
        required: Option<(usize, usize)>,
    ) -> ReqKind {
        // If the pages the caller is *blocked on* are already being fetched, wait on that request
        // and send nothing at all. Returning its key is the whole mechanism: `add_request` finds it
        // by exact match and hands back an inflight that sends nothing, and the wait loop rechecks
        // this caller's own required pages, not the covering request's.
        //
        // Testing the whole range instead (below) is far stricter than it looks. The widening turns
        // a one-page touch into ~900, so a fault landing inside a request already in flight almost
        // never has its *widened* range covered, while almost always having its required pages
        // covered -- and so sends a second ~900-page request overlapping the first nearly
        // completely. That is where the duplicates surviving covering-request coalescing came from.
        //
        // What this gives up is that fault's read-ahead: its speculative pages are never asked for.
        // `installed` in the profile is the check on whether that costs anything -- it held flat at
        // ~11.5k when duplicate transfer was first removed, and a drop here would mean this bought
        // fewer duplicates with less coverage. Hence the switch: one rebuild reverts it.
        if Self::COALESCE_ON_REQUIRED {
            // Only when `required` actually falls in this range. `ensure_in_core` hands the same
            // required range to every sub-request of a split, and a sub-range that does not contain
            // it is pure speculation that nobody is waiting for -- coalescing that onto an
            // unrelated request would drop the read-ahead without anyone having asked to wait.
            if let Some((rp, rl)) =
                required.filter(|(p, l)| *p < start + len && *p + *l > start && *l > 0)
            {
                if let Some((_, key)) = self.covering_page_data(id, rp, rl) {
                    super::profile::PAGER_PROFILE.covered_required();
                    return key;
                }
            }
        }

        // Cover before narrowing, not after. Narrowing can never trim below `required`, so a range
        // one request already covers in full would come out as exactly the required pages and then
        // be *sent* -- a duplicate transfer of them, and a slot spent -- where returning the range
        // whole lets `add_request` attach the caller to the covering request and send nothing.
        if self.covering_page_data(id, start, len).is_some() {
            return ReqKind::new_page_data(id, start, len, flags);
        }
        let (lo, hi) = self.narrow_page_data(id, start, len, required);
        if hi <= lo || (lo == start && hi == start + len) {
            return ReqKind::new_page_data(id, start, len, flags);
        }
        super::profile::PAGER_PROFILE.narrowed(len - (hi - lo));
        ReqKind::new_page_data(id, lo, hi - lo, flags)
    }

    /// An in-flight region sync for `id`, if any, as a waitable (non-sending) inflight. Full
    /// scan: the map holds at most NR_REQUESTS entries and this runs once per sync submission,
    /// which is milliseconds of io.
    pub fn find_sync_region(&self, id: ObjID) -> Option<Inflight> {
        self.req_map.iter().find_map(|req| match req.reqkind() {
            ReqKind::SyncRegion(info) if info.id == id => {
                Some(Inflight::new(req.id, req.reqkind().clone(), false))
            }
            _ => None,
        })
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
            super::profile::PAGER_PROFILE.coalesced();
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
                super::profile::PAGER_PROFILE.coalesced();
                return Ok(Inflight::new(req.id, twin, false));
            }
        }

        // A range something else is already fetching in full is not worth a second transfer: wait
        // on the request that covers it. Neither lookup above can see this -- an
        // overlapping range never compares equal to the range containing it -- and it is
        // where the duplicate pages in completions come from (`INPROG.md`: a fifth of what
        // the pager delivers). The waiting works out the same as the prefetch case: the
        // covering request's key is what `setup_wait` compares against and what its
        // completion is removed under.
        if let ReqKind::PageData(id, start, len, _) = &rk {
            if let Some((req_id, key)) = self.covering_page_data(*id, *start, *len) {
                log::trace!(
                    "request {:?} coalescing onto covering request {:?}",
                    rk,
                    key
                );
                super::profile::PAGER_PROFILE.covered();
                return Ok(Inflight::new(req_id, key, false));
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
            super::profile::PAGER_PROFILE.no_slot();
            return Err(rk);
        };
        // Every page-data request that goes out overlapping one already in flight is a duplicate
        // transfer waiting to happen, and by this point neither coalescing nor narrowing has been
        // able to prevent it. Naming both ranges is what says *which* askers they are -- a demand
        // fault's widened ~900 pages look nothing like a `required = None` caller (COW clone,
        // preload) asking for a whole object. A handful of lines a boot; diagnostic, remove after.
        if let ReqKind::PageData(oid, s, l, f) = &rk {
            let others = self.page_data_ranges(*oid);
            let overlapping: heapless::Vec<_, 8> = others
                .iter()
                .filter(|(a, b)| *a < s + l && a + b > *s)
                .collect();
            if !overlapping.is_empty() {
                log::info!(
                    "DUPSRC submit: obj {} sending [{}, +{}) flags {:?} while these overlap it: {:?}",
                    oid,
                    s,
                    l,
                    f,
                    overlapping
                );
            }
        }
        super::profile::PAGER_PROFILE.submitted(self.live, rk.all_pages().count());
        self.live += 1;
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
            // Before `mark_done`, which would erase the distinction: whether the page count ever
            // completed this request, and how much of what it asked for never arrived. Together
            // these say whether the read-ahead over-ask has a runtime consequence or is confined
            // to the accounting.
            super::profile::PAGER_PROFILE
                .fulfillment(request.was_done(), request.unfulfilled_pages());
            request.mark_done();
            request.signal();
            let age = request.age_ns();
            super::profile::PAGER_PROFILE.completed(age);
            // Only requests that were both sent and answered can be split; a request answered
            // without ever reaching the queue (an error, or a completion for something already
            // satisfied) has no pager segment to attribute.
            if let (Some(submitted), Some(first)) =
                (request.submitted_ns(), request.first_compl_ns())
            {
                super::profile::PAGER_PROFILE.completed_split(submitted, first, age);
            }
            self.live -= 1;
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
