use alloc::{collections::btree_set::BTreeSet, sync::Arc, vec::Vec};

use twizzler_abi::{
    object::ObjID,
    pager::{
        KernelCommand, ObjectEvictFlags, ObjectEvictInfo, ObjectRange, PagerFlags, PhysRange,
        RequestFromKernel,
    },
    syscall::{ObjectCreate, SyncInfo},
};

use crate::{
    memory::context::virtmem::region::Shadow,
    obj::{pages::PageRef, range::GetPageFlags, PageNumber},
    random::getrandom,
    sched::schedule_thread,
    thread::{CriticalGuard, ThreadRef},
};

#[derive(Debug, Clone)]
pub struct SyncRegionInfo {
    pub reqs: Arc<Vec<RequestFromKernel>>,
    shadow: Arc<Shadow>,
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

#[derive(Debug, Clone, PartialEq, PartialOrd, Eq, Ord)]
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
        id: ObjID,
        shadow: Shadow,
        mut dirty_set: Vec<PageNumber>,
        sync_info: SyncInfo,
        version: u64,
    ) -> Self {
        dirty_set.sort();

        let pages = shadow.with_page_tree(|page_tree| {
            dirty_set
                .iter()
                .filter_map(|dirty_page| {
                    match page_tree.try_get_page(*dirty_page, GetPageFlags::empty()) {
                        crate::obj::range::PageStatus::Ready(page_ref, _) => {
                            Some((*dirty_page, page_ref))
                        }
                        _ => {
                            logln!("warn -- no page found for page in dirty set");
                            None
                        }
                    }
                })
                .collect::<Vec<_>>()
        });

        fn consecutive_slices(
            data: &[(PageNumber, PageRef)],
        ) -> impl Iterator<Item = &[(PageNumber, PageRef)]> {
            let mut slice_start = 0;
            (1..=data.len()).flat_map(move |i| {
                if i == data.len()
                    || data[i - 1]
                        .1
                        .physical_address()
                        .offset(data[i - 1].1.nr_pages() * PageNumber::PAGE_SIZE)
                        .unwrap()
                        != data[i].1.physical_address()
                    || data[i - 1].0.next() != data[i].0.next()
                {
                    let begin = slice_start;
                    slice_start = i;
                    Some(&data[begin..i])
                } else {
                    None
                }
            })
        }

        let runs = consecutive_slices(pages.as_slice())
            .enumerate()
            .map(|(i, run)| {
                let is_last = i == pages.len() - 1;
                let range = ObjectRange::new(
                    run[0].0.as_byte_offset() as u64,
                    run.last()
                        .unwrap()
                        .0
                        .offset(run.last().unwrap().1.nr_pages())
                        .as_byte_offset() as u64,
                );

                let phys = PhysRange::new(
                    run[0].1.physical_address().raw(),
                    run.last()
                        .unwrap()
                        .1
                        .physical_address()
                        .offset(run.last().unwrap().1.nr_pages() * PageNumber::PAGE_SIZE)
                        .unwrap()
                        .raw(),
                );
                let flags = if is_last {
                    ObjectEvictFlags::SYNC | ObjectEvictFlags::FENCE
                } else {
                    ObjectEvictFlags::SYNC
                };
                log::debug!(
                    "sync object {:?} pages {:?} => {:?} (is last: {})",
                    id,
                    range,
                    phys,
                    is_last
                );
                RequestFromKernel::new(KernelCommand::ObjectEvict(ObjectEvictInfo::new(
                    id, range, phys, version, flags,
                )))
            });

        let mut unique = [0u8; 16];
        getrandom(&mut unique, false);
        let unique_id = u128::from_ne_bytes(unique) ^ id.raw();
        ReqKind::SyncRegion(SyncRegionInfo {
            reqs: Arc::new(runs.collect()),
            shadow: Arc::new(shadow),
            id,
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

    pub fn pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::PageData(_, start, len, _) => (*start..(*start + *len)).into_iter(),
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

pub struct Request {
    id: usize,
    reqkind: ReqKind,
    remaining_pages: BTreeSet<usize>,
    cmd_ready: bool,
    waiting_threads: Vec<ThreadRef>,
}

impl Request {
    pub fn new(id: usize, reqkind: ReqKind) -> Self {
        let mut remaining_pages = BTreeSet::new();
        for page in reqkind.pages() {
            remaining_pages.insert(page);
        }
        Self {
            id,
            cmd_ready: !reqkind.needs_cmd(),
            reqkind,
            waiting_threads: Vec::new(),
            remaining_pages,
        }
    }

    pub fn reqkind(&self) -> &ReqKind {
        &self.reqkind
    }

    pub fn done(&self) -> bool {
        self.cmd_ready && self.remaining_pages.is_empty()
    }

    pub fn signal(&mut self) {
        for thread in self.waiting_threads.drain(..) {
            schedule_thread(thread);
        }
    }

    pub fn cmd_ready(&mut self) {
        self.cmd_ready = true;
    }

    pub fn page_ready(&mut self, page: usize) {
        self.remaining_pages.remove(&page);
    }

    pub fn setup_wait<'a>(&mut self, thread: &'a ThreadRef) -> Option<CriticalGuard<'a>> {
        if self.done() {
            return None;
        }
        let critical = thread.enter_critical();
        self.waiting_threads.push(thread.clone());
        Some(critical)
    }
}
