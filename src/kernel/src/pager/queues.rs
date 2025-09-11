use alloc::sync::Arc;

use heapless::index_map::FnvIndexMap;
use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    pager::{
        CompletionToKernel, CompletionToPager, KernelCommand, KernelCompletionFlags, ObjectInfo,
        ObjectRange, PageFlags, PagerCompletionData, PagerRequest, PhysRange, RequestFromKernel,
        RequestFromPager,
    },
    syscall::{MapFlags, NANOS_PER_SEC},
};
use twizzler_rt_abi::error::{ObjectError, RawTwzError, TwzError};

use super::{inflight_mgr, provide_pager_memory, DEFAULT_PAGER_OUTSTANDING_FRAMES};
use crate::{
    arch::{memory::phys_to_virt, PhysAddr},
    idcounter::{IdCounter, SimpleId},
    instant::Instant,
    is_test_mode,
    memory::{
        context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
        pagetables::{ContiguousProvider, MappingCursor, MappingFlags, MappingSettings},
        sim_memory_pressure,
        tracker::start_reclaim_thread,
    },
    obj::{
        lookup_object,
        pages::{Page, PageRef},
        LookupFlags, Object, ObjectRef, PageNumber,
    },
    once::Once,
    queue::{ManagedQueueReceiver, QueueObject},
    security::KERNEL_SCTX,
    spinlock::Spinlock,
    thread::{
        current_thread_ref,
        entry::{run_closure_in_new_thread, start_new_kernel},
        priority::Priority,
    },
};

#[derive(Clone, Debug)]
struct SentRequestInfo {
    req: RequestFromKernel,
    obj: Option<ObjectRef>,
}

struct RequestSender {
    ids: IdCounter,
    queue: QueueObject<RequestFromKernel, CompletionToKernel>,
    idmap: Spinlock<heapless::index_map::FnvIndexMap<u32, SentRequestInfo, 1024>>,
}

static SENDER: Once<RequestSender> = Once::new();

static RECEIVER: Once<ManagedQueueReceiver<RequestFromPager, CompletionToPager>> = Once::new();

fn pager_request_copy_user_phys(
    target_object: ObjID,
    offset: usize,
    len: usize,
    phys: PhysRange,
    write_phys: bool,
) -> CompletionToPager {
    let Ok(phys_start) = PhysAddr::new(phys.start) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };

    let Ok(object) = lookup_object(target_object, LookupFlags::empty()).ok_or(()) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };
    let ko = kernel_context().insert_kernel_object::<()>(ObjectContextInfo::new(
        object,
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
        MapFlags::empty(),
    ));
    let Ok(vaddr) = ko.start_addr().offset(offset) else {
        return CompletionToPager::new(PagerCompletionData::Error(
            TwzError::INVALID_ARGUMENT.into(),
        ));
    };

    let vphys = phys_start.kernel_vaddr();
    let user_slice = unsafe { core::slice::from_raw_parts_mut(vaddr.as_mut_ptr(), len) };
    let phys_slice =
        unsafe { core::slice::from_raw_parts_mut(vphys.as_mut_ptr::<u8>(), phys.len()) };

    let copy_len = core::cmp::min(user_slice.len(), phys_slice.len());
    let (target_slice, source_slice) = if write_phys {
        (phys_slice, user_slice)
    } else {
        (user_slice, phys_slice)
    };
    target_slice[0..copy_len].copy_from_slice(&source_slice[0..copy_len]);
    target_slice[copy_len..].fill(0);

    CompletionToPager::new(PagerCompletionData::Okay)
}

fn pager_register_phys(phys: u64, len: u64) -> Result<(), TwzError> {
    log::info!("register phys: {:x} - {:x}", phys, phys + len);
    let paddr = PhysAddr::new(phys).map_err(|_| TwzError::INVALID_ARGUMENT)?;
    let vaddr = phys_to_virt(paddr);
    let cursor = MappingCursor::new(vaddr, len as usize);
    let settings = MappingSettings::new(
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
        MappingFlags::GLOBAL,
    );
    let mut phys = ContiguousProvider::new(paddr, len as usize, settings);
    kernel_context().with_arch(KERNEL_SCTX, |arch| arch.map(cursor, &mut phys));
    Ok(())
}

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|_id, req| match req.cmd() {
            PagerRequest::Ready => {
                log::debug!("pager ready");
                inflight_mgr().lock().set_ready();
                provide_pager_memory(DEFAULT_PAGER_OUTSTANDING_FRAMES, false);

                start_reclaim_thread();
                log::debug!("reclaim thread started");
                // TODO
                if is_test_mode() && false {
                    run_closure_in_new_thread(Priority::USER, || {
                        sim_memory_pressure();
                    });
                }

                CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Okay)
            }
            PagerRequest::CopyUserPhys {
                target_object,
                offset,
                len,
                phys,
                write_phys,
            } => pager_request_copy_user_phys(target_object, offset, len, phys, write_phys),
            PagerRequest::RegisterPhys(phys, len) => match pager_register_phys(phys, len) {
                Ok(_) => CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Okay),
                Err(e) => CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Error(
                    RawTwzError::new(e.raw()),
                )),
            },
        });
    }
}

fn pager_compl_handle_page_data(
    obj: &ObjectRef,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    flags: PageFlags,
) {
    let start = Instant::now();
    let pcount = phys_range.page_count();
    log::trace!(
        "got : {} {:?} {:?} ({} pages)",
        obj.id(),
        obj_range,
        phys_range,
        pcount
    );

    if !flags.contains(PageFlags::WIRED) {
        log::trace!(
            "untrack {:?} from pager memory ({} pages, pager has {} pages left)",
            phys_range,
            pcount,
            crate::memory::tracker::get_outstanding_pager_pages()
        );
        crate::memory::tracker::untrack_page_pager(pcount);
        if crate::memory::tracker::get_outstanding_pager_pages()
            < DEFAULT_PAGER_OUTSTANDING_FRAMES / 2
        {
            super::provide_pager_memory(DEFAULT_PAGER_OUTSTANDING_FRAMES, false);
        }
    }
    let done_mem = Instant::now();
    let mut done_lock = Instant::zero();
    let mut done_calc = Instant::zero();
    let mut done_new = Instant::zero();
    let mut done_add = Instant::zero();
    let mut done_drop = Instant::zero();

    let mut count = 0;
    let max_obj = obj_range.page_count();
    let max_phys = phys_range.page_count();
    while count < max_obj {
        let objpage_nr = obj_range.pages().nth(count).unwrap();
        let physpage_nr = phys_range.pages().nth(count).unwrap();

        let pn = PageNumber::from(objpage_nr as usize);
        let pa = PhysAddr::new(physpage_nr * PageNumber::PAGE_SIZE as u64).unwrap();

        let thiscount = (max_obj - count).min(max_phys - count);
        done_calc = Instant::now();
        let page = if flags.contains(PageFlags::WIRED) {
            log::trace!("wiring {} pages: {}", thiscount, objpage_nr);
            Page::new_wired(pa, PageNumber::PAGE_SIZE * thiscount, CacheType::WriteBack)
        } else {
            if let Some(frame) = crate::memory::frame::get_frame(pa) {
                Page::new(frame, thiscount)
            } else {
                log::warn!(
                    "non-wired physical address, but not known by frame allocator: {:?}",
                    pa
                );
                Page::new_wired(pa, PageNumber::PAGE_SIZE * thiscount, CacheType::WriteBack)
            }
        };

        let page = PageRef::new(Arc::new(page), 0, thiscount);
        done_new = Instant::now();
        log::trace!(
            "Adding page {}: {} {} {:?} {:?}",
            obj.id(),
            pn,
            thiscount,
            page.physical_address(),
            flags
        );
        let mut object_tree = obj.lock_page_tree();
        done_lock = Instant::now();
        object_tree.add_page(pn, page, None);
        let done_add = Instant::now();
        drop(object_tree);
        let done_drop = Instant::now();
        count += thiscount;
    }

    //inflight_mgr()
    //.lock()
    //.pages_ready(objid, obj_range.pages().map(|x| x as usize));
    inflight_mgr()
        .lock()
        .pages_ready(obj.id(), obj_range.pages().next().map(|x| x as usize));
    let done_signal = Instant::now();
    log::info!(
        "::: {}ns {}ns {}ns {}ns {}ns {}ns {}ns",
        (done_mem - start).as_nanos(),
        (done_calc - done_mem).as_nanos(),
        (done_new - done_calc).as_nanos(),
        (done_lock - done_new).as_nanos(),
        (done_add - done_lock).as_nanos(),
        (done_drop - done_add).as_nanos(),
        (done_signal - done_drop).as_nanos(),
    );
}

fn pager_compl_handle_object_info(id: ObjID, info: ObjectInfo) {
    let obj = Object::new(id, info.lifetime, &[]);
    crate::obj::register_object(Arc::new(obj));
    inflight_mgr().lock().cmd_ready(id, false);
}

fn pager_compl_handle_error(request: RequestFromKernel, err: TwzError) {
    logln!("pager returned error: {} for {:?}", err, request);
    match err {
        TwzError::Object(ObjectError::NoSuchObject) => {
            if let KernelCommand::ObjectInfoReq(obj_id) = request.cmd() {
                crate::obj::no_exist(obj_id);
                inflight_mgr().lock().cmd_ready(obj_id, false);
            }
        }
        _ => {}
    }
}

pub(super) fn pager_compl_handler_main() {
    let sender = SENDER.wait();

    let mut count = 0;
    let mut elapsed = 0;
    let mut last_ticks;
    let mut current_ticks = None;
    loop {
        last_ticks = current_ticks;
        current_ticks = crate::time::bench_clock().map(|bc| bc.read());
        let completion = sender.queue.recv_completion();
        log::info!(
            "{}: got completion {:?}",
            current_thread_ref().unwrap().id(),
            completion
        );

        count += 1;

        if let Some(current_ticks) = current_ticks {
            if let Some(last_ticks) = last_ticks {
                elapsed += (current_ticks.as_nanos() - last_ticks.as_nanos()) as u64;
            }
        }

        if elapsed >= NANOS_PER_SEC {
            log::trace!(
                "pager completion thread processed {} entries over the last {}ms",
                count,
                elapsed / (NANOS_PER_SEC / 1000),
            );
            count = 0;
            elapsed = 0;
        }

        let Some(request) = sender.idmap.lock().get(&completion.0).cloned() else {
            logln!("warn -- received completion for unknown request");
            continue;
        };

        match completion.1.data() {
            twizzler_abi::pager::KernelCompletionData::PageDataCompletion(
                objid,
                obj_range,
                phys_range,
                flags,
            ) => pager_compl_handle_page_data(
                request.obj.as_ref().unwrap(),
                obj_range,
                phys_range,
                flags,
            ),
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(id, info) => {
                pager_compl_handle_object_info(id, info)
            }
            twizzler_abi::pager::KernelCompletionData::Error(err) => {
                pager_compl_handle_error(request.req, err.error())
            }
            _ => {}
        };
        log::info!("done handling completion");

        match request.req.cmd() {
            KernelCommand::ObjectEvict(info) => {
                if matches!(
                    completion.1.data(),
                    twizzler_abi::pager::KernelCompletionData::Okay
                ) {
                    inflight_mgr().lock().cmd_ready(info.obj_id, true);
                }
            }
            _ => {}
        }

        if completion.1.flags().contains(KernelCompletionFlags::DONE) {
            log::info!("removing completion");
            sender.idmap.lock().remove(&completion.0);
            sender.ids.release_simple(SimpleId::from(completion.0));
            log::info!("done removing completion");
        }
    }
}

pub fn submit_pager_request(req: RequestFromKernel, obj: Option<&ObjectRef>) {
    log::info!("submitting pager request: {:?}", req);
    let sender = SENDER.wait();
    let id = sender.ids.next_simple().value() as u32;
    let old = sender
        .idmap
        .lock()
        .insert(
            id,
            SentRequestInfo {
                req,
                obj: obj.cloned(),
            },
        )
        .unwrap();
    if let Some(old) = old {
        logln!(
            "warn -- replaced old item on request index ({}: {:?} -> {:?})",
            id,
            old,
            req
        );
    }
    SENDER.wait().queue.submit(req, id);
}

extern "C" fn pager_compl_handler_entry() {
    pager_compl_handler_main();
}

extern "C" fn pager_request_handler_entry() {
    pager_request_handler_main();
}

pub fn init_pager_queue(id: ObjID, outgoing: bool) {
    let obj = match lookup_object(id, LookupFlags::empty()) {
        crate::obj::LookupResult::Found(o) => o,
        _ => panic!("pager queue not found"),
    };
    log::debug!(
        "[kernel::pager] registered {} pager queue: {}",
        if outgoing { "sender" } else { "receiver" },
        id
    );
    if outgoing {
        let queue = QueueObject::<RequestFromKernel, CompletionToKernel>::from_object(obj);
        SENDER.call_once(|| RequestSender {
            ids: IdCounter::new(),
            queue,
            idmap: Spinlock::new(FnvIndexMap::new()),
        });
    } else {
        let queue = QueueObject::<RequestFromPager, CompletionToPager>::from_object(obj);
        let receiver = ManagedQueueReceiver::new(queue);
        RECEIVER.call_once(|| receiver);
    }
    if SENDER.poll().is_some() && RECEIVER.poll().is_some() {
        // TODO: these should be higher?
        start_new_kernel(Priority::REALTIME, pager_compl_handler_entry, 0);
        start_new_kernel(Priority::USER, pager_request_handler_entry, 0);
        log::debug!("pager queues and handlers initialized");
    }
}
