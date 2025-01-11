use alloc::{collections::btree_set::BTreeSet, vec::Vec};

use twizzler_abi::object::ObjID;

use crate::{
    sched::schedule_thread,
    thread::{CriticalGuard, ThreadRef},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReqKind {
    Info(ObjID),
    PageData(ObjID, usize, usize),
    Sync(ObjID),
    Del(ObjID),
    Create(ObjID),
}

impl ReqKind {
    pub fn new_info(obj_id: ObjID) -> Self {
        ReqKind::Info(obj_id)
    }

    pub fn new_page_data(obj_id: ObjID, start: usize, len: usize) -> Self {
        ReqKind::PageData(obj_id, start, len)
    }

    pub fn new_sync(obj_id: ObjID) -> Self {
        ReqKind::Sync(obj_id)
    }

    pub fn new_del(obj_id: ObjID) -> Self {
        ReqKind::Del(obj_id)
    }

    pub fn new_create(obj_id: ObjID) -> Self {
        ReqKind::Create(obj_id)
    }

    pub fn pages(&self) -> impl Iterator<Item = usize> {
        match self {
            ReqKind::PageData(_, start, len) => (*start..(*start + *len)).into_iter(),
            _ => (0..0).into_iter(),
        }
    }

    pub fn needs_info(&self) -> bool {
        matches!(self, ReqKind::Info(_))
    }

    pub fn needs_sync(&self) -> bool {
        matches!(self, ReqKind::Sync(_)) || matches!(self, ReqKind::Del(_))
    }

    pub fn objid(&self) -> ObjID {
        match self {
            ReqKind::Info(obj_id) => *obj_id,
            ReqKind::PageData(obj_id, _, _) => *obj_id,
            ReqKind::Sync(obj_id) => *obj_id,
            ReqKind::Del(obj_id) => *obj_id,
            ReqKind::Create(obj_id) => *obj_id,
        }
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
            reqkind,
            cmd_ready: !(reqkind.needs_info() || reqkind.needs_sync()),
            waiting_threads: Vec::new(),
            remaining_pages,
        }
    }

    pub fn reqkind(&self) -> ReqKind {
        self.reqkind
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
