use alloc::vec::Vec;

use inflight::InflightManager;
use request::ReqKind;
use twizzler_abi::{object::ObjID, pager::PhysRange};

use crate::{
    memory::{frame::PHYS_LEVEL_LAYOUTS, tracker::FrameAllocFlags},
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

pub const MAX_PAGER_OUTSTANDING_FRAMES: usize = 65536;
pub const DEFAULT_PAGER_OUTSTANDING_FRAMES: usize = 1024;

static INFLIGHT_MGR: Once<Mutex<InflightManager>> = Once::new();

fn inflight_mgr() -> &'static Mutex<InflightManager> {
    INFLIGHT_MGR.call_once(|| Mutex::new(InflightManager::new()))
}

pub fn lookup_object_and_wait(id: ObjID) -> Option<ObjectRef> {
    loop {
        match crate::obj::lookup_object(id, LookupFlags::empty()) {
            crate::obj::LookupResult::Found(arc) => return Some(arc),
            crate::obj::LookupResult::WasDeleted => return None,
            _ => {}
        }

        let mut mgr = inflight_mgr().lock();
        if !mgr.is_ready() {
            return None;
        }
        let inflight = mgr.add_request(ReqKind::new_info(id));
        drop(mgr);
        if let Some(pager_req) = inflight.pager_req() {
            queues::submit_pager_request(pager_req);
        }

        let mut mgr = inflight_mgr().lock();
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
            drop(mgr);
            finish_blocking(guard);
        };
    }
}

pub fn get_page_and_wait(id: ObjID, page: PageNumber) {
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return;
    }
    let inflight = mgr.add_request(ReqKind::new_page_data(id, page.num(), 1));
    drop(mgr);
    if let Some(pager_req) = inflight.pager_req() {
        queues::submit_pager_request(pager_req);
    }

    let mut mgr = inflight_mgr().lock();
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        finish_blocking(guard);
    };
}

fn cmd_object(req: ReqKind) {
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return;
    }
    let inflight = mgr.add_request(req);
    drop(mgr);
    if let Some(pager_req) = inflight.pager_req() {
        queues::submit_pager_request(pager_req);
    }

    let mut mgr = inflight_mgr().lock();
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

fn get_memory_for_pager(min_frames: usize) -> Vec<PhysRange> {
    let mut ranges = Vec::new();
    let mut count = 0;
    if crate::memory::tracker::get_outstanding_pager_pages() + min_frames
        >= MAX_PAGER_OUTSTANDING_FRAMES
    {
        return Vec::new();
    }
    while count < min_frames {
        let req_max = (min_frames - count).min(DEFAULT_PAGER_OUTSTANDING_FRAMES);
        let level = if req_max * PHYS_LEVEL_LAYOUTS[0].size() >= PHYS_LEVEL_LAYOUTS[1].size() {
            1
        } else {
            0
        };

        if let Some(frame) = crate::memory::tracker::try_alloc_frame(
            FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[level],
        ) {
            count += PHYS_LEVEL_LAYOUTS[1].size() / PHYS_LEVEL_LAYOUTS[0].size();
            crate::memory::tracker::track_page_pager(frame);
            ranges.push(PhysRange::new(
                frame.start_address().raw(),
                frame.start_address().offset(frame.size()).unwrap().raw(),
            ));
        } else {
            if let Some(frame) = crate::memory::tracker::try_alloc_frame(
                FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            ) {
                count += 1;
                crate::memory::tracker::track_page_pager(frame);
                ranges.push(PhysRange::new(
                    frame.start_address().raw(),
                    frame.start_address().offset(frame.size()).unwrap().raw(),
                ));
            }
        }
    }
    ranges
}

pub fn provide_pager_memory(min_frames: usize, wait: bool) {
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return;
    }
    //print_tracker_stats();
    let ranges = get_memory_for_pager(min_frames);
    logln!(
        "allocated {} ranges for pager (min_frames = {}, total = {} KB)",
        ranges.len(),
        min_frames,
        ranges.iter().fold(0, |acc, x| acc + x.len()) / 1024
    );
    //print_tracker_stats();

    let inflights = ranges
        .iter()
        .map(|range| {
            let req = ReqKind::new_pager_memory(*range);
            mgr.add_request(req)
        })
        .collect::<Vec<_>>();

    drop(mgr);

    for inflight in &inflights {
        if let Some(pager_req) = inflight.pager_req() {
            queues::submit_pager_request(pager_req);
        }
    }

    if wait {
        for inflight in &inflights {
            let mut mgr = inflight_mgr().lock();
            let thread = current_thread_ref().unwrap();
            if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
                drop(mgr);
                finish_blocking(guard);
            };
        }
    }
}
