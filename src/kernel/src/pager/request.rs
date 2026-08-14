use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::{KeyAdapter, LinkedList, RBTreeAtomicLink, intrusive_adapter};
use twizzler_abi::{
    object::ObjID,
    pager::{
        KernelCommand, ObjectEvictFlags, ObjectEvictInfo, ObjectRange, PagerFlags, PhysRange,
        RequestFromKernel,
    },
    syscall::ObjectCreate,
};
use twizzler_rt_abi::bindings::sync_info;

use crate::{
    arch::PhysAddr,
    instant::Instant,
    obj::{ObjectRef, PageNumber, pagetables::DirtyList},
    spinlock::Spinlock,
    syscall::sync::{add_all_to_requeue, claim_own_wakeup, requeue_all},
    thread::{CriticalGuard, Thread, ThreadRef, current_thread_ref},
};

#[derive(Debug, Clone)]
pub struct SyncRegionInfo {
    pub reqs: Arc<Vec<RequestFromKernel>>,
    pub id: ObjID,
    pub unique_id: ObjID,
    pub sync_info: Option<sync_info>,
    pub dirty: Arc<DirtyList>,
}

impl PartialEq for SyncRegionInfo {
    fn eq(&self, other: &Self) -> bool {
        self.unique_id.eq(&other.unique_id)
    }
}

impl Eq for SyncRegionInfo {}

impl PartialOrd for SyncRegionInfo {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        self.unique_id.partial_cmp(&other.unique_id)
    }
}

impl Ord for SyncRegionInfo {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.unique_id.cmp(&other.unique_id)
    }
}

/// `ReqKind` is the key of `InflightManager::req_map`, an `RBTree`, and that tree uses **both**
/// comparison traits for different operations: `insert` descends with `key < current_key`
/// (`PartialOrd`), while `find`/`find_mut` descend with `key.cmp(current_key)` (`Ord`). They must
/// therefore agree, so both are derived.
///
/// This used to carry a hand-written `PartialOrd` whose `PageData` arm treated two requests as
/// equal when their ranges overlapped, while `Ord` stayed derived (field-wise on
/// `(id, start, len, flags)`). Nothing noticed for as long as every live `PageData` key was either
/// identical or disjoint, because the two orders only disagree on overlap. Speculative prefetch is
/// the first thing that ever puts overlapping-but-unequal keys in the tree at once -- a prefetch of
/// a whole region alongside a demand fault inside it -- and at that point a node is *placed* where
/// the search will never look for it. `remove_request` then silently removes nothing: the request
/// stays in the map forever, its waiter is never signalled, and its slot is never freed. That is
/// the wedge recorded in `pagerperf.md` 18, and it is why it produced no panic and no timeout.
///
/// Note this loses no coalescing that was ever in effect: `add_request` looks up with `find`, which
/// has always used the derived order, so overlapping requests never coalesced regardless. Doing it
/// for real needs an explicit range scan (`lower_bound` over the same object), not a comparator --
/// overlap-equality is not transitive and cannot order a search tree.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReqKind {
    Info(ObjID),
    PageData(ObjID, usize, usize, PagerFlags),
    Sync(ObjID),
    SyncRegion(SyncRegionInfo),
    Del(ObjID),
    Create(ObjID, ObjectCreate, u128),
    Pages(PhysRange),
}

impl ReqKind {
    pub fn new_info(obj_id: ObjID) -> Self {
        ReqKind::Info(obj_id)
    }

    pub fn new_page_data(obj_id: ObjID, start: usize, len: usize, flags: PagerFlags) -> Self {
        ReqKind::PageData(obj_id, start, len, flags)
    }

    pub fn new_sync(obj_id: ObjID) -> Self {
        ReqKind::Sync(obj_id)
    }

    pub fn new_sync_region(
        object: &ObjectRef,
        dirty: DirtyList,
        sync_info: Option<sync_info>,
        version: u64,
    ) -> Self {
        fn consecutive_slices(
            data: &[(PageNumber, PhysAddr, usize)],
        ) -> impl Iterator<Item = &[(PageNumber, PhysAddr, usize)]> {
            let mut slice_start = 0;
            (1..=data.len()).flat_map(move |i| {
                if i == data.len()
                    || data[i - 1]
                        .1
                        .offset(PageNumber::PAGE_SIZE * data[i - 1].2)
                        .unwrap()
                        != data[i].1
                    || data[i - 1].0.offset(data[i - 1].2) != data[i].0
                {
                    let begin = slice_start;
                    slice_start = i;
                    Some(&data[begin..i])
                } else {
                    None
                }
            })
        }

        static COUNTER_1: AtomicU64 = AtomicU64::new(1);
        let unique_id = COUNTER_1.fetch_add(1, Ordering::Relaxed) as u128;

        let slices = consecutive_slices(dirty.pages()).collect::<Vec<_>>();
        let runs = slices.iter().enumerate().map(|(i, run)| {
            let is_last = i == slices.len() - 1;
            let first = &run[0];
            let last = run.last().unwrap();
            let range = ObjectRange::new(
                first.0.as_byte_offset() as u64,
                last.0.offset(last.2).as_byte_offset() as u64,
            );

            let phys = PhysRange::new(
                first.1.raw(),
                last.1.offset(PageNumber::PAGE_SIZE * last.2).unwrap().raw(),
            );
            let flags = if is_last {
                ObjectEvictFlags::SYNC | ObjectEvictFlags::FENCE
            } else {
                ObjectEvictFlags::SYNC
            };
            log::trace!(
                "sync object {:?} pages {:?} => {:?} (is last: {})",
                object.id(),
                range,
                phys,
                is_last
            );
            RequestFromKernel::new(KernelCommand::ObjectEvict(ObjectEvictInfo::new(
                object.id(),
                range,
                phys,
                version,
                flags,
                unique_id.into(),
            )))
        });

        ReqKind::SyncRegion(SyncRegionInfo {
            reqs: Arc::new(runs.collect()),
            id: object.id(),
            unique_id: unique_id.into(),
            sync_info,
            dirty: Arc::new(dirty),
        })
    }

    pub fn new_del(obj_id: ObjID) -> Self {
        ReqKind::Del(obj_id)
    }

    pub fn new_create(obj_id: ObjID, create: &ObjectCreate, nonce: u128) -> Self {
        ReqKind::Create(obj_id, *create, nonce)
    }

    pub fn new_pager_memory(range: PhysRange) -> Self {
        ReqKind::Pages(range)
    }

    /// The key a speculative request for this exact range would have, if this one is not already
    /// speculative.
    ///
    /// [PagerFlags::PREFETCH] is part of the coalescing key, so a prefetch and the demand fault it
    /// was issued to pre-empt are two entries covering one range -- two transfers, the second of
    /// which `Table::map` throws away (`pagerperf.md` 18's `DUP_PAGES`). It has to stay in the key,
    /// because the pager routes and caps on it. So the demand side looks for the speculative twin
    /// explicitly instead.
    pub fn prefetch_twin(&self) -> Option<ReqKind> {
        match self {
            ReqKind::PageData(id, start, len, flags) if !flags.contains(PagerFlags::PREFETCH) => {
                Some(ReqKind::PageData(
                    *id,
                    *start,
                    *len,
                    *flags | PagerFlags::PREFETCH,
                ))
            }
            _ => None,
        }
    }

    pub fn all_pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::PageData(_, start, len, _flags) => (*start..(*start + *len)).into_iter(),
            _ => (0..0).into_iter(),
        }
    }

    pub fn required_pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::PageData(_, start, _len, flags) if !flags.contains(PagerFlags::PREFETCH) => {
                (*start..(*start + 1)).into_iter()
            }
            _ => (0..0).into_iter(),
        }
    }

    pub fn needs_info(&self) -> bool {
        matches!(self, ReqKind::Info(_)) || matches!(self, ReqKind::Create(_, _, _))
    }

    pub fn needs_sync(&self) -> bool {
        matches!(self, ReqKind::Sync(_))
            || matches!(self, ReqKind::Del(_))
            || matches!(self, ReqKind::SyncRegion(_))
    }

    pub fn needs_cmd(&self) -> bool {
        self.needs_sync() || self.needs_info()
    }

    pub fn objid(&self) -> Option<ObjID> {
        Some(match self {
            ReqKind::Info(obj_id) => *obj_id,
            ReqKind::PageData(obj_id, _, _, _) => *obj_id,
            ReqKind::Sync(obj_id) => *obj_id,
            ReqKind::SyncRegion(info) => info.id,
            ReqKind::Del(obj_id) => *obj_id,
            ReqKind::Create(obj_id, _, _) => *obj_id,
            ReqKind::Pages(_) => return None,
        })
    }
}

intrusive_adapter!(pub RequestMapAdapter = &'static Request : Request { link: intrusive_collections::rbtree::AtomicLink });

intrusive_adapter!(pub PagerLinkAdapter = ThreadRef : Thread { pager_link: intrusive_collections::linked_list::AtomicLink });

pub struct Request {
    pub id: usize,
    reqkind: ReqKind,
    waiters: Spinlock<LinkedList<PagerLinkAdapter>>,
    remaining_pages: AtomicUsize,
    done: AtomicBool,
    start_time: Instant,
    /// Nanoseconds after `start_time` at which the request reached the queue, and at which its
    /// first completion was handled. Zero means "not yet". Offsets rather than `Instant`s so they
    /// can be stamped through `&self`, which is all the map hands out.
    submitted_ns: AtomicU64,
    first_compl_ns: AtomicU64,
    link: RBTreeAtomicLink,
}

impl<'a> KeyAdapter<'a> for RequestMapAdapter {
    type Key = &'a ReqKind;
    fn get_key(&self, s: &'a Request) -> &'a ReqKind {
        &s.reqkind
    }
}

impl Request {
    pub fn new(id: usize, reqkind: ReqKind) -> Self {
        let start_time = Instant::now();
        Self {
            id,
            remaining_pages: AtomicUsize::new(reqkind.all_pages().count()),
            reqkind,
            waiters: Spinlock::new(LinkedList::new(PagerLinkAdapter::NEW)),
            done: AtomicBool::new(false),
            start_time,
            submitted_ns: AtomicU64::new(0),
            first_compl_ns: AtomicU64::new(0),
            link: RBTreeAtomicLink::new(),
        }
    }

    /// Nanoseconds since this request was created.
    pub fn age_ns(&self) -> u64 {
        (Instant::now() - self.start_time).as_nanos() as u64
    }

    /// Stamps the moment this request reached the queue. Only the first call counts: one request
    /// can produce several wire requests, and the segment being measured ends at the first of them.
    pub fn mark_submitted(&self) {
        Self::stamp(&self.submitted_ns, self.age_ns());
    }

    /// Stamps the first completion handled for this request.
    pub fn mark_first_completion(&self) {
        Self::stamp(&self.first_compl_ns, self.age_ns());
    }

    fn stamp(slot: &AtomicU64, ns: u64) {
        // `max(1)` keeps a sub-nanosecond stamp from reading as unset.
        let _ = slot.compare_exchange(0, ns.max(1), Ordering::Relaxed, Ordering::Relaxed);
    }

    /// Nanoseconds from creation to queue submit, once submitted.
    pub fn submitted_ns(&self) -> Option<u64> {
        Some(self.submitted_ns.load(Ordering::Relaxed)).filter(|ns| *ns != 0)
    }

    /// Nanoseconds from creation to the first completion, once one has landed.
    pub fn first_compl_ns(&self) -> Option<u64> {
        Some(self.first_compl_ns.load(Ordering::Relaxed)).filter(|ns| *ns != 0)
    }

    pub fn is_timed_out(&self) -> bool {
        let elapsed = Instant::now() - self.start_time;
        elapsed.as_secs() >= 2
    }

    pub fn reqkind(&self) -> &ReqKind {
        &self.reqkind
    }

    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn finished_pages(&self, count: usize) -> bool {
        loop {
            let current = self.remaining_pages.load(Ordering::Acquire);
            if current < count {
                self.remaining_pages.store(0, Ordering::Release);
                return true;
            }
            if self
                .remaining_pages
                .compare_exchange(current, current - count, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return current - count == 0;
            }
        }
    }

    pub fn mark_done(&self) {
        if !self.done() {
            log::trace!(
                "request {} ({:?}) took {}us",
                self.id,
                self.reqkind(),
                (Instant::now() - self.start_time).as_micros()
            );
        }
        self.done.store(true, Ordering::Release);
    }

    pub fn signal(&self) {
        let g = current_thread_ref().unwrap().enter_critical();
        let mut waiters = self.waiters.lock();
        add_all_to_requeue(waiters.take().into_iter());
        requeue_all();
        drop(waiters);
        drop(g);
    }

    pub fn setup_wait<'a>(&self, thread: &'a ThreadRef) -> Option<CriticalGuard<'a>> {
        if self.done() {
            return None;
        }
        let critical = thread.enter_critical();
        self.waiters.lock().push_back(thread.clone());
        thread.set_sync_sleep_done();
        // The guard the thread-sync sleep paths take and this one never had. A waker that parked a
        // requeue entry for us before the flag went up has already done its half: blocking now
        // waits on an unrelated `requeue_all()`, and being woken by one leaves us still linked in
        // `waiters` -- which is the state the *next* `setup_wait` panics on ("attempted to insert
        // an object that is already linked").
        if claim_own_wakeup(thread) {
            let unlinked = thread.pager_link.is_linked().then(|| {
                // Sound: we pushed this thread onto this list just above, and nothing else links
                // `pager_link` -- a `signal` racing us would have taken it off instead.
                let mut waiters = self.waiters.lock();
                unsafe { waiters.cursor_mut_from_ptr(&**thread).remove() }
            });
            // Outside the spinlock: dropping a ThreadRef can release the thread's id, which takes
            // a sleeping mutex.
            drop(unlinked);
            return None;
        }
        Some(critical)
    }
}
