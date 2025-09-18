use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use intrusive_collections::{intrusive_adapter, KeyAdapter, LinkedList, RBTreeAtomicLink};
use twizzler_abi::{
    object::ObjID,
    pager::{
        KernelCommand, ObjectEvictFlags, ObjectEvictInfo, ObjectRange, PagerFlags, PhysRange,
        RequestFromKernel,
    },
    syscall::{ObjectCreate, SyncInfo},
};

use crate::{
    arch::PhysAddr,
    condvar::CondVarLinkAdapter,
    instant::Instant,
    memory::context::virtmem::region::Shadow,
    obj::{range::GetPageFlags, ObjectRef, PageNumber},
    processor::sched::schedule_thread,
    random::getrandom,
    spinlock::Spinlock,
    thread::{CriticalGuard, ThreadRef},
};

#[derive(Debug, Clone)]
pub struct SyncRegionInfo {
    pub reqs: Arc<Vec<RequestFromKernel>>,
    shadow: Option<Arc<Shadow>>,
    pub id: ObjID,
    pub unique_id: ObjID,
    pub sync_info: SyncInfo,
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

impl PartialOrd for ReqKind {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        let self_disc = match self {
            ReqKind::Info(_) => 0,
            ReqKind::PageData(_, _, _, _) => 1,
            ReqKind::Sync(_) => 2,
            ReqKind::SyncRegion(_) => 3,
            ReqKind::Del(_) => 4,
            ReqKind::Create(_, _, _) => 5,
            ReqKind::Pages(_) => 6,
        };

        let other_disc = match other {
            ReqKind::Info(_) => 0,
            ReqKind::PageData(_, _, _, _) => 1,
            ReqKind::Sync(_) => 2,
            ReqKind::SyncRegion(_) => 3,
            ReqKind::Del(_) => 4,
            ReqKind::Create(_, _, _) => 5,
            ReqKind::Pages(_) => 6,
        };

        let disc = self_disc.partial_cmp(&other_disc).unwrap();
        if disc.is_eq() {
            Some(match (self, other) {
                (ReqKind::Info(self_id), ReqKind::Info(other_id)) => self_id.cmp(other_id),
                (
                    ReqKind::PageData(self_id, self_start, self_len, _),
                    ReqKind::PageData(other_id, other_start, _, _),
                ) => {
                    // We count two requests as equal if they overlap.
                    if *self_id == *other_id {
                        if *other_start >= *self_start && *other_start < *self_start + *self_len {
                            core::cmp::Ordering::Equal
                        } else {
                            self_start.cmp(other_start)
                        }
                    } else {
                        self_id.cmp(other_id)
                    }
                }
                (ReqKind::Sync(self_id), ReqKind::Sync(other_id)) => self_id.cmp(other_id),
                (ReqKind::SyncRegion(self_info), ReqKind::SyncRegion(other_info)) => {
                    self_info.cmp(other_info)
                }
                (ReqKind::Del(self_id), ReqKind::Del(other_id)) => self_id.cmp(other_id),
                (ReqKind::Create(self_id, _, _), ReqKind::Create(other_id, _, _)) => {
                    self_id.cmp(other_id)
                }
                (ReqKind::Pages(self_range), ReqKind::Pages(other_range)) => {
                    self_range.cmp(other_range)
                }

                _ => unreachable!(),
            })
        } else {
            Some(disc)
        }
    }
}

impl PartialEq for ReqKind {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}

#[derive(Debug, Clone, Eq, Ord)]
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
        shadow: Option<Shadow>,
        dirty_set: &[(PageNumber, usize)],
        sync_info: SyncInfo,
        version: u64,
    ) -> Self {
        let mut page_tree = object.lock_page_tree();
        let pages = dirty_set
            .iter()
            .flat_map(|dirty_page| {
                let mut pages = Vec::new();
                let mut off = 0;
                while off < dirty_page.1 {
                    match page_tree.try_get_page(dirty_page.0.offset(off), GetPageFlags::empty()) {
                        crate::obj::range::PageStatus::Ready(page_ref, _) => {
                            pages.push((
                                dirty_page.0.offset(off),
                                page_ref.physical_address(),
                                page_ref.nr_pages(),
                            ));
                            off += page_ref.nr_pages();
                        }
                        _ => {
                            logln!("warn -- no page found for page in dirty set");
                            off += 1;
                        }
                    }
                }
                pages
            })
            .collect::<Vec<_>>();
        drop(page_tree);

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

        let slices = consecutive_slices(pages.as_slice()).collect::<Vec<_>>();
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
            )))
        });

        let mut unique = [0u8; 16];
        getrandom(&mut unique, false);
        let unique_id = u128::from_ne_bytes(unique) ^ object.id().raw();
        ReqKind::SyncRegion(SyncRegionInfo {
            reqs: Arc::new(runs.collect()),
            shadow: shadow.map(Arc::new),
            id: object.id(),
            unique_id: unique_id.into(),
            sync_info,
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

    pub fn all_pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::PageData(_, start, len, flags) if !flags.contains(PagerFlags::PREFETCH) => {
                (*start..(*start + *len)).into_iter()
            }
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

pub struct Request {
    pub id: usize,
    reqkind: ReqKind,
    waiters: Spinlock<LinkedList<CondVarLinkAdapter>>,
    remaining_pages: AtomicUsize,
    done: AtomicBool,
    start_time: Instant,
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
            waiters: Spinlock::new(LinkedList::new(CondVarLinkAdapter::NEW)),
            done: AtomicBool::new(false),
            start_time,
            link: RBTreeAtomicLink::new(),
        }
    }

    pub fn reqkind(&self) -> &ReqKind {
        &self.reqkind
    }

    pub fn done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn finished_pages(&self, count: usize) -> bool {
        let old = self.remaining_pages.fetch_sub(count, Ordering::SeqCst);
        assert!(old >= count);
        old - count == 0
    }

    pub fn mark_done(&self) {
        if !self.done() {
            log::debug!(
                "request {} ({:?}) took {}us",
                self.id,
                self.reqkind(),
                (Instant::now() - self.start_time).as_micros()
            );
        }
        self.done.store(true, Ordering::Release);
    }

    pub fn signal(&self) {
        let mut waiters = self.waiters.lock();
        while let Some(thread) = waiters.pop_front() {
            schedule_thread(thread);
        }
    }

    pub fn setup_wait<'a>(&self, thread: &'a ThreadRef) -> Option<CriticalGuard<'a>> {
        if self.done() {
            return None;
        }
        let critical = thread.enter_critical();
        self.waiters.lock().push_back(thread.clone());
        Some(critical)
    }
}
