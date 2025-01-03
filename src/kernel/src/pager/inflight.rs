use alloc::collections::{btree_map::BTreeMap, btree_set::BTreeSet};

use stable_vec::StableVec;
use twizzler_abi::{object::ObjID, pager::RequestFromKernel};

use super::{request::ReqKind, Request};
use crate::thread::{CriticalGuard, ThreadRef};

pub struct Inflight {
    id: usize,
    rk: ReqKind,
}

impl Inflight {
    pub(super) fn new(id: usize, rk: ReqKind) -> Self {
        Self { id, rk }
    }

    pub(super) fn pager_req(&self) -> Option<RequestFromKernel> {
        todo!()
    }
}

#[derive(Default)]
struct PerObjectData {
    page_map: BTreeMap<usize, BTreeSet<usize>>,
    info_list: BTreeSet<usize>,
}

impl PerObjectData {
    fn insert(&mut self, rk: ReqKind, id: usize) {
        for page in rk.pages() {
            self.page_map.entry(page).or_default().insert(id);
        }
        if rk.needs_info() {
            self.info_list.insert(id);
        }
    }

    fn remove_all(&mut self, rk: ReqKind, id: usize) {
        for page in rk.pages() {
            self.page_map.entry(page).or_default().remove(&id);
        }
        if rk.needs_info() {
            self.info_list.remove(&id);
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
            return Inflight::new(*id, rk);
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
        Inflight::new(id, rk)
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

    pub fn info_ready(&mut self, objid: ObjID) {
        if let Some(po) = self.per_object.get_mut(&objid) {
            for id in &po.info_list {
                if let Some(req) = self.requests.get_mut(*id) {
                    req.info_ready();
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
                        } else {
                            logln!("[pager] warning -- stale ID");
                        }
                    }
                }
            }
        }
    }
}
