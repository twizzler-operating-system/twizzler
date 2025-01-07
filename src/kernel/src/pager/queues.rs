use twizzler_abi::{
    device::CacheType,
    object::{ObjID, Protections},
    pager::{
        CompletionToKernel, CompletionToPager, KernelCommand, ObjectRange, PagerCompletionData,
        PagerRequest, PhysRange, RequestFromKernel, RequestFromPager,
    },
};

use super::request::ReqKind;
use crate::{
    arch::{PhysAddr, VirtAddr},
    idcounter::{IdCounter, SimpleId},
    memory::context::{kernel_context, KernelMemoryContext, ObjectContextInfo},
    obj::{lookup_object, LookupFlags},
    once::Once,
    pager::PAGER_MEMORY,
    queue::{ManagedQueueReceiver, ManagedQueueSender, QueueObject},
    syscall::object::sys_sctx_attach,
    thread::{
        current_memory_context, current_thread_ref, entry::start_new_kernel, priority::Priority,
    },
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

    logln!("lookup {}", target_object);
    let Ok(object) = lookup_object(target_object, LookupFlags::empty()).ok_or(()) else {
        return CompletionToPager::new(PagerCompletionData::Error);
    };
    logln!("found object! {}", object.id());
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

fn pager_test_request() -> CompletionToPager {
    let sender = SENDER.wait();
    let obj_id = ObjID::new(1001);
    logln!("kernel: submitting page data request on K2P Queue");
    let item = RequestFromKernel::new(twizzler_abi::pager::KernelCommand::PageDataReq(
        obj_id,
        ObjectRange {
            start: 0,
            end: 4096,
        },
    ));
    let id = sender.0.next_simple().value() as u32;
    let res = SENDER.wait().1.submit(item, id);

    logln!("kernel: submitting obj info request on K2P Queue");
    let item = RequestFromKernel::new(twizzler_abi::pager::KernelCommand::ObjectInfoReq(obj_id));
    let id = sender.0.next_simple().value() as u32;
    let res = SENDER.wait().1.submit(item, id);

    return CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::TestResp);
}

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|id, req| {
            logln!("kernel: got req {}:{:?} from pager", id, req);
            match req.cmd() {
                PagerRequest::EchoReq => {
                    CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::EchoResp)
                }
                PagerRequest::TestReq => pager_test_request(),
                PagerRequest::Ready => {
                    let reg = PAGER_MEMORY
                        .poll()
                        .map(|pm| (pm.start.raw(), pm.length))
                        .unwrap_or((0, 0));
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
            }
        });
    }
}

pub(super) fn pager_compl_handler_main() {
    let sender = SENDER.wait();
    loop {
        let completion = sender.1.recv_completion();
        match completion.1.data() {
            twizzler_abi::pager::KernelCompletionData::EchoResp => {
                logln!("got echo response");
            }
            twizzler_abi::pager::KernelCompletionData::PageDataCompletion(phys_range) => {
                logln!("got physical range {:?}", phys_range);
            }
            twizzler_abi::pager::KernelCompletionData::ObjectInfoCompletion(obj_info) => {
                logln!("got object info {:?}", obj_info);
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
        "[kernel-pager] registered {} pager queue: {}",
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
