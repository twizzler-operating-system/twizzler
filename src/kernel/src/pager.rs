use alloc::vec::Vec;

use inflight::InflightManager;
use request::ReqKind;
use twizzler_abi::object::ObjID;

use crate::{
    memory::{MemoryRegion, MemoryRegionKind},
    mutex::Mutex,
    obj::{LookupFlags, ObjectRef, PageNumber},
    once::Once,
    syscall::sync::finish_blocking,
    thread::current_thread_ref,
};

mod inflight;
mod queues;
mod request;

pub use queues::init_pager_queue;
pub use request::Request;

static PAGER_MEMORY: Once<MemoryRegion> = Once::new();

pub fn pager_select_memory_regions(regions: &[MemoryRegion]) -> Vec<MemoryRegion> {
    let mut fa_regions = Vec::new();
    for reg in regions {
        if matches!(reg.kind, MemoryRegionKind::UsableRam) {
            // TODO: don't just pick one, and don't just pick the first one.
            if PAGER_MEMORY.poll().is_none() {
                PAGER_MEMORY.call_once(|| *reg);
            } else {
                fa_regions.push(*reg);
            }
        }
    }
    fa_regions
}

lazy_static::lazy_static! {
    static ref INFLIGHT_MGR: Mutex<InflightManager> = Mutex::new(InflightManager::new());
}

pub fn lookup_object_and_wait(id: ObjID) -> Option<ObjectRef> {
    loop {
        match crate::obj::lookup_object(id, LookupFlags::empty()) {
            crate::obj::LookupResult::Found(arc) => return Some(arc),
            _ => {}
        }

        let mut mgr = INFLIGHT_MGR.lock();
        if !mgr.is_ready() {
            return None;
        }
        let inflight = mgr.add_request(ReqKind::new_info(id));
        drop(mgr);
        if let Some(pager_req) = inflight.pager_req() {
            queues::submit_pager_request(pager_req);
        }

        let mut mgr = INFLIGHT_MGR.lock();
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
            drop(mgr);
            finish_blocking(guard);
        };
    }
}

pub fn get_page_and_wait(id: ObjID, page: PageNumber) {
    let mut mgr = INFLIGHT_MGR.lock();
    if !mgr.is_ready() {
        return;
    }
    let inflight = mgr.add_request(ReqKind::new_page_data(id, page.num(), 1));
    drop(mgr);
    if let Some(pager_req) = inflight.pager_req() {
        queues::submit_pager_request(pager_req);
    }

    let mut mgr = INFLIGHT_MGR.lock();
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        finish_blocking(guard);
    };
}

fn cmd_object(req: ReqKind) {
    let mut mgr = INFLIGHT_MGR.lock();
    if !mgr.is_ready() {
        return;
    }
    let inflight = mgr.add_request(req);
    drop(mgr);
    if let Some(pager_req) = inflight.pager_req() {
        queues::submit_pager_request(pager_req);
    }

    let mut mgr = INFLIGHT_MGR.lock();
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        finish_blocking(guard);
    };
}

pub fn sync_object(id: ObjID) {
    cmd_object(ReqKind::new_sync(id));
}

pub fn del_object(id: ObjID) {
    cmd_object(ReqKind::new_del(id));
}

pub fn create_object(id: ObjID) {
    cmd_object(ReqKind::new_create(id));
}

pub fn ensure_in_core(obj: &ObjectRef, start: PageNumber, len: usize) {
    if !obj.use_pager() {
        return;
    }
    for i in 0..len {
        let page = start.offset(i);
        get_page_and_wait(obj.id(), page);
    }
}

pub fn get_object_page(obj: &ObjectRef, pn: PageNumber) {
    ensure_in_core(obj, pn, 1);
}
