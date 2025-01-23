use alloc::sync::Arc;

use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, NULLPAGE_SIZE},
    pager::{
        CompletionToKernel, CompletionToPager, PagerCompletionData, PagerRequest, PhysRange,
        RequestFromKernel, RequestFromPager,
    },
    syscall::LifetimeType,
};

use super::INFLIGHT_MGR;
use crate::{
    arch::PhysAddr,
    idcounter::{IdCounter, SimpleId},
    memory::context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
    obj::{lookup_object, pages::Page, LookupFlags, Object, PageNumber},
    once::Once,
    pager::PAGER_MEMORY,
    queue::{ManagedQueueReceiver, QueueObject},
    thread::{entry::start_new_kernel, priority::Priority},
};

static SENDER: Once<(
    IdCounter,
    QueueObject<RequestFromKernel, CompletionToKernel>,
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
        return CompletionToPager::new(PagerCompletionData::Error);
    };

    let Ok(object) = lookup_object(target_object, LookupFlags::empty()).ok_or(()) else {
        return CompletionToPager::new(PagerCompletionData::Error);
    };
    let ko = kernel_context().insert_kernel_object::<()>(ObjectContextInfo::new(
        object,
        Protections::READ | Protections::WRITE,
        CacheType::WriteBack,
    ));
    let Ok(vaddr) = ko.start_addr().offset(offset) else {
        return CompletionToPager::new(PagerCompletionData::Error);
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

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|_id, req| match req.cmd() {
            PagerRequest::Ready => {
                let reg = PAGER_MEMORY
                    .poll()
                    .map(|pm| (pm[0].start.raw(), pm[0].length))
                    .unwrap_or((0, 0));
                INFLIGHT_MGR.lock().set_ready();
                CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::DramPages(
                    PhysRange::new(reg.0, reg.0 + reg.1 as u64),
                ))
            }
            PagerRequest::CopyUserPhys {
                target_object,
                offset,
                len,
                phys,
                write_phys,
            } => pager_request_copy_user_phys(target_object, offset, len, phys, write_phys),
        });
    }
}

pub(super) fn pager_compl_handler_main() {
    let sender = SENDER.wait();
    loop {
        let completion = sender.1.recv_completion();
        match completion.1.data() {
            twizzler_abi::pager::KernelCompletionData::PageDataCompletion(
                objid,
                obj_range,
                phys_range,
            ) => {
                if let Ok(object) = lookup_object(objid, LookupFlags::empty()).ok_or(()) {
                    let mut object_tree = object.lock_page_tree();

                    for (objpage_nr, physpage_nr) in obj_range.pages().zip(phys_range.pages()) {
                        let pn = PageNumber::from(objpage_nr as usize);
                        let pa = PhysAddr::new(physpage_nr * NULLPAGE_SIZE as u64).unwrap();
                        object_tree.add_page(pn, Page::new_wired(pa, CacheType::WriteBack));
                    }
                    drop(object_tree);

                    INFLIGHT_MGR
                        .lock()
                        .pages_ready(objid, obj_range.pages().map(|x| x as usize));
                } else {
                    logln!("kernel: pager: got unknown object ID");
                }
            }
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(obj_info) => {
                let obj = Object::new(obj_info.obj_id, LifetimeType::Persistent);
                crate::obj::register_object(Arc::new(obj));
                INFLIGHT_MGR.lock().cmd_ready(obj_info.obj_id, false);
            }
            twizzler_abi::pager::KernelCompletionData::SyncOkay(objid) => {
                INFLIGHT_MGR.lock().cmd_ready(objid, true);
            }
            twizzler_abi::pager::KernelCompletionData::Error => {
                logln!("pager returned error");
            }
            twizzler_abi::pager::KernelCompletionData::NoSuchObject(obj_id) => {
                logln!(
                    "kernel: pager compl: got object info {:?}: no such object",
                    obj_id
                );
                crate::obj::no_exist(obj_id);
                INFLIGHT_MGR.lock().cmd_ready(obj_id, false);
            }
        }
        sender.0.release_simple(SimpleId::from(completion.0));
    }
}

pub fn submit_pager_request(item: RequestFromKernel) {
    let sender = SENDER.wait();
    let id = sender.0.next_simple().value() as u32;
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
    logln!(
        "[kernel::pager] registered {} pager queue: {}",
        if outgoing { "sender" } else { "receiver" },
        id
    );
    if outgoing {
        let queue = QueueObject::<RequestFromKernel, CompletionToKernel>::from_object(obj);
        SENDER.call_once(|| (IdCounter::new(), queue));
    } else {
        let queue = QueueObject::<RequestFromPager, CompletionToPager>::from_object(obj);
        let receiver = ManagedQueueReceiver::new(queue);
        RECEIVER.call_once(|| receiver);
    }
    if SENDER.poll().is_some() && RECEIVER.poll().is_some() {
        start_new_kernel(Priority::default_user(), pager_compl_handler_entry, 0);
        start_new_kernel(Priority::default_user(), pager_request_handler_entry, 0);
    }
}
