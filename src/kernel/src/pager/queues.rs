use twizzler_abi::{
    object::ObjID,
    pager::{
        CompletionToKernel, CompletionToPager, KernelCommand, RequestFromKernel, RequestFromPager,
    },
};

use super::request::ReqKind;
use crate::{
    idcounter::{IdCounter, SimpleId},
    obj::{lookup_object, LookupFlags},
    once::Once,
    queue::{ManagedQueueReceiver, ManagedQueueSender, QueueObject},
    thread::{entry::start_new_kernel, priority::Priority},
};

static SENDER: Once<(
    IdCounter,
    QueueObject<RequestFromKernel, CompletionToKernel>,
)> = Once::new();
static RECEIVER: Once<ManagedQueueReceiver<RequestFromPager, CompletionToPager>> = Once::new();

pub(super) fn pager_request_handler_main() {
    let receiver = RECEIVER.wait();
    loop {
        receiver.handle_request(|id, req| {
            logln!("kernel: got req {}:{:?} from pager", id, req);
            CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::EchoResp)
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
