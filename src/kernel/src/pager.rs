use alloc::vec::Vec;

use inflight::InflightManager;
use request::ReqKind;
use twizzler_abi::object::ObjID;

use crate::{
    memory::{MemoryRegion, MemoryRegionKind},
    mutex::Mutex,
    obj::{LookupFlags, ObjectRef},
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
                logln!("selecting pager region {:?}", reg);
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
        logln!("trying to lookup info about object {}", id);

        match crate::obj::lookup_object(id, LookupFlags::empty()) {
            crate::obj::LookupResult::Found(arc) => return Some(arc),
            _ => {}
        }

        let mut mgr = INFLIGHT_MGR.lock();
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

fn cmd_object(req: ReqKind) {
    let mut mgr = INFLIGHT_MGR.lock();
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
