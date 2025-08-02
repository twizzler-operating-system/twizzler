use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, NULLPAGE_SIZE},
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
    mutex::Mutex,
    obj::{
        lookup_object,
        pages::{Page, PageRef},
        LookupFlags, Object, PageNumber,
    },
    once::Once,
    queue::{ManagedQueueReceiver, QueueObject},
    security::KERNEL_SCTX,
    thread::{
        entry::{run_closure_in_new_thread, start_new_kernel},
        priority::Priority,
    },
};

static SENDER: Once<(
    IdCounter,
    QueueObject<RequestFromKernel, CompletionToKernel>,
    Mutex<BTreeMap<u32, RequestFromKernel>>,
)> = Once::new();
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
    objid: ObjID,
    obj_range: ObjectRange,
    phys_range: PhysRange,
    flags: PageFlags,
) {
    let pcount = phys_range.pages().count();
    log::trace!("got : {} {:?} {:?}", objid, obj_range, phys_range);

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

    if let Ok(object) = lookup_object(objid, LookupFlags::empty()).ok_or(()) {
        let mut object_tree = object.lock_page_tree();

        let start = Instant::now();
        let mut count = 0;
        let max_obj = obj_range.pages().count();
        let max_phys = phys_range.pages().count();
        while count < max_obj {
            let objpage_nr = obj_range.pages().nth(count).unwrap();
            let physpage_nr = phys_range.pages().nth(count).unwrap();

            let pn = PageNumber::from(objpage_nr as usize);
            let pa = PhysAddr::new(physpage_nr * NULLPAGE_SIZE as u64).unwrap();

            let (page, thiscount) = if flags.contains(PageFlags::WIRED) {
                let max_pages = (max_obj - count).min(max_phys - count);
                log::info!("wiring {} pages: {}", max_pages, objpage_nr);
                (
                    Page::new_wired(pa, PageNumber::PAGE_SIZE * max_pages, CacheType::WriteBack),
                    max_pages,
                )
            } else {
                (
                    if let Some(frame) = crate::memory::frame::get_frame(pa) {
                        Page::new(frame)
                    } else {
                        log::warn!(
                            "non-wired physical address, but not known by frame allocator: {:?}",
                            pa
                        );
                        Page::new_wired(pa, PageNumber::PAGE_SIZE, CacheType::WriteBack)
                    },
                    1,
                )
            };
            if thiscount > 1 {
                log::info!("pre");
                object_tree.print_tree();
            }
            let page = PageRef::new(Arc::new(page), 0, thiscount);
            object_tree.add_page(pn, page, None);

            if thiscount > 1 {
                log::info!("post");
                object_tree.print_tree();
            }
            count += thiscount;
        }
        drop(object_tree);

        inflight_mgr()
            .lock()
            .pages_ready(objid, obj_range.pages().map(|x| x as usize));
        let end = Instant::now();
        log::info!(
            "processed {} pages in {} us",
            obj_range.pages().count(),
            (end - start).as_micros()
        );
    } else {
        // TODO
        logln!("kernel: pager: got unknown object ID");
    }
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
        let completion = sender.1.recv_completion();

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

        //log::info!("got: {:?}", completion);
        let Some(request) = sender.2.lock().get(&completion.0).copied() else {
            logln!("warn -- received completion for unknown request");
            continue;
        };

        match completion.1.data() {
            twizzler_abi::pager::KernelCompletionData::PageDataCompletion(
                objid,
                obj_range,
                phys_range,
                flags,
            ) => pager_compl_handle_page_data(objid, obj_range, phys_range, flags),
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(id, info) => {
                pager_compl_handle_object_info(id, info)
            }
            twizzler_abi::pager::KernelCompletionData::Error(err) => {
                pager_compl_handle_error(request, err.error())
            }
            _ => {}
        };

        match request.cmd() {
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
            sender.2.lock().remove(&completion.0);
            sender.0.release_simple(SimpleId::from(completion.0));
        }
    }
}

pub fn submit_pager_request(item: RequestFromKernel) {
    log::trace!("submitting pager request: {:?}", item);
    let sender = SENDER.wait();
    let id = sender.0.next_simple().value() as u32;
    let old = sender.2.lock().insert(id, item);
    if let Some(old) = old {
        logln!(
            "warn -- replaced old item on request index ({}: {:?} -> {:?})",
            id,
            old,
            item
        );
    }
    SENDER.wait().1.submit(item, id);
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
        SENDER.call_once(|| (IdCounter::new(), queue, Mutex::new(BTreeMap::new())));
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
