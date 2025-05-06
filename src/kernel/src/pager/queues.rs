use alloc::{collections::btree_map::BTreeMap, sync::Arc};

use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections, NULLPAGE_SIZE},
    pager::{
        CompletionToKernel, CompletionToPager, KernelCommand, PagerCompletionData, PagerRequest,
        PhysRange, RequestFromKernel, RequestFromPager,
    },
    syscall::LifetimeType,
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::INFLIGHT_MGR;
use crate::{
    arch::PhysAddr,
    idcounter::{IdCounter, SimpleId},
    memory::context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
    mutex::Mutex,
    obj::{lookup_object, pages::Page, LookupFlags, Object, PageNumber},
    once::Once,
    queue::{ManagedQueueReceiver, QueueObject},
    thread::{entry::start_new_kernel, priority::Priority},
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

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|_id, req| match req.cmd() {
            PagerRequest::Ready => {
                /*
                                let regs = PAGER_MEMORY.poll().unwrap();
                                for reg in regs {
                                    submit_pager_request(RequestFromKernel::new(KernelCommand::DramPages(
                                        PhysRange::new(
                                            reg.start.raw(),
                                            reg.start.offset(reg.length).unwrap().raw(),
                                        ),
                                    )));
                                }
                */
                // TODO
                INFLIGHT_MGR.lock().set_ready();
                CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::Okay)
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
        let Some(request) = sender.2.lock().get(&completion.0).copied() else {
            logln!("warn -- received completion for unknown request");
            continue;
        };

        let done = match completion.1.data() {
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
                        // TODO: will need to supply allocator
                        object_tree.add_page(pn, Page::new_wired(pa, CacheType::WriteBack), None);
                    }
                    drop(object_tree);

                    INFLIGHT_MGR
                        .lock()
                        .pages_ready(objid, obj_range.pages().map(|x| x as usize));
                } else {
                    logln!("kernel: pager: got unknown object ID");
                }
                false
            }
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(_) => {
                if let Some(id) = request.id() {
                    let obj = Object::new(id, LifetimeType::Persistent, &[]);
                    crate::obj::register_object(Arc::new(obj));
                    INFLIGHT_MGR.lock().cmd_ready(id, false);
                }
                false
            }
            twizzler_abi::pager::KernelCompletionData::Error(err) => {
                logln!("pager returned error: {}", err.error());
                match err.error() {
                    TwzError::Object(ObjectError::NoSuchObject) => {
                        if let KernelCommand::ObjectInfoReq(obj_id) = request.cmd() {
                            crate::obj::no_exist(obj_id);
                            INFLIGHT_MGR.lock().cmd_ready(obj_id, false);
                        }
                    }
                    _ => {}
                }
                true
            }
            twizzler_abi::pager::KernelCompletionData::Okay => {
                match request.cmd() {
                    KernelCommand::ObjectEvict(info) => {
                        INFLIGHT_MGR.lock().cmd_ready(info.obj_id, true);
                    }
                    _ => {}
                }
                true
            }
        };
        if done {
            sender.2.lock().remove(&completion.0);
            sender.0.release_simple(SimpleId::from(completion.0));
        }
    }
}

pub fn submit_pager_request(item: RequestFromKernel) {
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
    logln!(
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
        start_new_kernel(Priority::USER, pager_compl_handler_entry, 0);
        start_new_kernel(Priority::USER, pager_request_handler_entry, 0);
    }
}
