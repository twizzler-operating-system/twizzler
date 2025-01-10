use alloc::{collections::btree_set::BTreeSet, vec::Vec};

use twizzler_abi::object::ObjID;

use crate::{
    sched::schedule_thread,
    thread::{CriticalGuard, ThreadRef},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum ReqKind {
    Info(ObjID),
    PageData(ObjID, usize, usize),
}

impl ReqKind {
    pub fn new_info(obj_id: ObjID) -> Self {
        ReqKind::Info(obj_id)
    }

    pub fn new_page_data(obj_id: ObjID, start: usize, len: usize) -> Self {
        ReqKind::PageData(obj_id, start, len)
    }

    pub fn pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::Info(_) => (0..0).into_iter(),
            ReqKind::PageData(_, start, len) => (*start..(*start + *len)).into_iter(),
        }
    }

    pub fn needs_info(&self) -> bool {
        matches!(self, ReqKind::Info(_))
    }

    pub fn objid(&self) -> ObjID {
        match self {
            ReqKind::Info(obj_id) => *obj_id,
            ReqKind::PageData(obj_id, _, _) => *obj_id,
        }
    }
}

pub struct Request {
    id: usize,
    reqkind: ReqKind,
    remaining_pages: BTreeSet<usize>,
    info_ready: Option<bool>,
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
            reqkind,
            info_ready: if reqkind.needs_info() {
                Some(false)
            } else {
                None
            },
            waiting_threads: Vec::new(),
            remaining_pages,
        }
    }

    pub fn reqkind(&self) -> ReqKind {
        self.reqkind
    }

    pub fn done(&self) -> bool {
        self.info_ready.unwrap_or(true) && self.remaining_pages.is_empty()
    }

    pub fn signal(&mut self) {
        for thread in self.waiting_threads.drain(..) {
            schedule_thread(thread);
        }
    }

    pub fn info_ready(&mut self) {
        self.info_ready.as_mut().map(|b| *b = true);
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
