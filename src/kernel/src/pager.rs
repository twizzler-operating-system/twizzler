use alloc::vec::Vec;

use inflight::InflightManager;
use request::ReqKind;
use twizzler_abi::{
    object::{ObjID, MAX_SIZE},
    pager::{PagerFlags, PhysRange},
    syscall::{ObjectCreate, SyncInfo},
};

use crate::{
    memory::{
        context::virtmem::region::{MapRegion, Shadow},
        frame::PHYS_LEVEL_LAYOUTS,
        tracker::FrameAllocFlags,
    },
    mutex::Mutex,
    obj::{LookupFlags, ObjectRef, PageNumber},
    once::Once,
    processor::sched::{schedule, SchedFlags},
    syscall::sync::finish_blocking,
    thread::current_thread_ref,
};

mod inflight;
mod queues;
mod request;

pub use queues::init_pager_queue;
pub use request::Request;

pub const MAX_PAGER_OUTSTANDING_FRAMES: usize = 65536;
pub const DEFAULT_PAGER_OUTSTANDING_FRAMES: usize = 1024 * 16;

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
        let Some(inflight) = mgr.add_request(ReqKind::new_info(id)) else {
            log::warn!("out of pager request slots");
            drop(mgr);
            schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
            continue;
        };
        drop(mgr);
        inflight.for_each_pager_req(|pager_req| {
            queues::submit_pager_request(pager_req, None, inflight.rk.clone());
        });

        let mut mgr = inflight_mgr().lock();
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
            drop(mgr);
            finish_blocking(guard);
        };
    }
}

pub fn get_pages_and_wait(
    obj: &ObjectRef,
    page: PageNumber,
    len: usize,
    flags: PagerFlags,
) -> bool {
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return false;
    }
    log::trace!(
        "{}: getting page {} from {}",
        current_thread_ref().unwrap().id(),
        page,
        obj.id()
    );
    let Some(inflight) = mgr.add_request(ReqKind::new_page_data(obj.id(), page.num(), len, flags))
    else {
        log::warn!("out of pager request slots");
        drop(mgr);
        schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        return get_pages_and_wait(obj, page, len, flags);
    };
    drop(mgr);
    let mut submitted = false;
    inflight.for_each_pager_req(|pager_req| {
        submitted = true;
        queues::submit_pager_request(pager_req, Some(obj), inflight.rk.clone());
    });

    if !flags.contains(PagerFlags::PREFETCH) {
        let mut mgr = inflight_mgr().lock();
        let thread = current_thread_ref().unwrap();
        if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
            drop(mgr);
            finish_blocking(guard);
        };
    }
    submitted
}

fn cmd_object(req: ReqKind, obj: Option<&ObjectRef>) {
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return;
    }
    let Some(inflight) = mgr.add_request(req.clone()) else {
        log::warn!("out of pager request slots");
        drop(mgr);
        schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        return cmd_object(req, obj);
    };
    drop(mgr);
    inflight.for_each_pager_req(|pager_req| {
        queues::submit_pager_request(pager_req, obj, inflight.rk.clone());
    });

    let mut mgr = inflight_mgr().lock();
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        finish_blocking(guard);
    };
}

pub fn sync_object(obj: &ObjectRef) {
    cmd_object(ReqKind::new_sync(obj.id()), Some(obj));
}

pub fn del_object(id: ObjID) {
    cmd_object(ReqKind::new_del(id), None);
}

pub fn create_object(id: ObjID, create: &ObjectCreate, nonce: u128) {
    cmd_object(ReqKind::new_create(id, create, nonce), None);
}

pub fn sync_region(
    region: &MapRegion,
    dirty_set: &[(PageNumber, usize)],
    sync_info: SyncInfo,
    version: u64,
) {
    // TODO: need to use shadow mapping to ensure that the pager sees a consistent mapping.
    let _shadow = Shadow::from(region);
    let req = ReqKind::new_sync_region(region.object(), None, dirty_set, sync_info, version);
    let mut mgr = inflight_mgr().lock();
    if !mgr.is_ready() {
        return;
    }
    let Some(inflight) = mgr.add_request(req) else {
        log::warn!("out of pager request slots");
        drop(mgr);
        schedule(SchedFlags::YIELD | SchedFlags::REINSERT);
        return sync_region(region, dirty_set, sync_info, version);
    };
    drop(mgr);
    inflight.for_each_pager_req(|pager_req| {
        queues::submit_pager_request(pager_req, Some(&region.object()), inflight.rk.clone());
    });

    let mut mgr = inflight_mgr().lock();
    let thread = current_thread_ref().unwrap();
    if let Some(guard) = mgr.setup_wait(&inflight, &thread) {
        drop(mgr);
        finish_blocking(guard);
    };
}

pub fn ensure_in_core(obj: &ObjectRef, start: PageNumber, len: usize, flags: PagerFlags) -> bool {
    if !obj.use_pager() {
        return false;
    }

    let avail_pager_mem = crate::memory::tracker::get_outstanding_pager_pages();
    let needed_additional =
        DEFAULT_PAGER_OUTSTANDING_FRAMES.saturating_sub(avail_pager_mem.saturating_sub(len));
    let wait_for_additional =
        avail_pager_mem.saturating_sub(len) < DEFAULT_PAGER_OUTSTANDING_FRAMES / 2;
    let low_mem = crate::memory::tracker::is_low_mem();

    log::trace!(
        "ensure in core {}: {}, {} pages (avail = {}, needed = {}, wait = {}, is_low_mem = {})",
        obj.id(),
        start.num(),
        len,
        avail_pager_mem,
        needed_additional,
        wait_for_additional,
        low_mem,
    );

    if flags.contains(PagerFlags::PREFETCH) && low_mem {
        return false;
    }

    if needed_additional > DEFAULT_PAGER_OUTSTANDING_FRAMES / 8 && !low_mem {
        provide_pager_memory(needed_additional, wait_for_additional);
    }

    get_pages_and_wait(obj, start, len, flags)
}

// Returns true if the pager was engaged.
pub fn get_object_page(obj: &ObjectRef, pn: PageNumber) -> bool {
    let max = PageNumber::from_offset(MAX_SIZE);
    if pn >= max {
        log::warn!("invalid page number: {:?}", pn);
    }
    let count_to_end = max - pn;
    let count = count_to_end.min(1024);

    let tree = obj.lock_page_tree();
    let mut range = tree.range(pn..pn.offset(count));
    let first_present = range.next();

    let count = if let Some(first_present) = first_present {
        if first_present.0.num() <= pn.num() {
            1
        } else {
            log::debug!(
                "found partial in check for range {:?}: {:?}",
                pn..pn.offset(count),
                first_present.0
            );
            first_present.0.num().saturating_sub(pn.num())
        }
    } else {
        count_to_end.min(1024)
    }
    .min(1024);
    log::trace!(
        "get page: {} {:?} {}",
        pn,
        first_present.map(|f| f.1.range()),
        count
    );
    drop(tree);
    if count == 0 {
        return false;
    }
    ensure_in_core(obj, pn, count, PagerFlags::empty())
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

        if let Some((frame, len)) = crate::memory::tracker::try_alloc_split_frames(
            FrameAllocFlags::ZEROED,
            PHYS_LEVEL_LAYOUTS[level],
        ) {
            let thiscount = len / PHYS_LEVEL_LAYOUTS[0].size();
            count += thiscount;
            crate::memory::tracker::track_page_pager(thiscount);
            ranges.push(PhysRange::new(
                frame.start_address().raw(),
                frame.start_address().offset(len).unwrap().raw(),
            ));
        } else {
            if let Some(frame) = crate::memory::tracker::try_alloc_frame(
                FrameAllocFlags::ZEROED,
                PHYS_LEVEL_LAYOUTS[0],
            ) {
                count += 1;
                crate::memory::tracker::track_page_pager(1);
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
    log::trace!(
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
            mgr.add_request(req).expect("TODO")
        })
        .collect::<Vec<_>>();

    drop(mgr);

    for inflight in &inflights {
        inflight.for_each_pager_req(|pager_req| {
            log::trace!("providing: {:?}", pager_req);
            queues::submit_pager_request(pager_req, None, inflight.rk.clone());
        });
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
