use twizzler_abi::{
    object::ObjID,
    pager::{CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager},
    thread::ExecutionState,
};

use crate::{
    obj::{lookup_object, LookupFlags},
    queue::{ManagedQueueReceiver, ManagedQueueSender, QueueObject},
    sched::schedule,
    thread::{current_thread_ref, entry::start_new_kernel, priority::Priority},
};

struct PagerQueues {
    sender: Option<ManagedQueueSender<RequestFromKernel, CompletionToKernel>>,
    receiver: Option<ManagedQueueReceiver<RequestFromPager, CompletionToPager>>,
}

static mut PAGER_QUEUES: PagerQueues = PagerQueues {
    sender: None,
    receiver: None,
};

extern "C" fn pager_entry() {
    pager_main();
}

extern "C" fn pager_compl_handler_entry() {
    pager_compl_handler_main();
}

extern "C" fn pager_request_handler_entry() {
    pager_request_handler_main();
}

fn pager_request_handler_main() {
    let receiver = unsafe { PAGER_QUEUES.receiver.as_ref().unwrap() };
    loop {
        receiver.handle_request(|id, req| {
            logln!("kernel: got req {}:{:?} from pager", id, req);
            CompletionToPager::new(twizzler_abi::pager::PagerCompletionData::EchoResp)
        });
    }
}

fn pager_compl_handler_main() {
    let sender = unsafe { PAGER_QUEUES.sender.as_ref().unwrap() };
    loop {
        sender.process_completion();
    }
}

fn pager_main() {
    logln!("kernel: hello from pager thread");
    let sender = unsafe { PAGER_QUEUES.sender.as_ref().unwrap() };
    loop {
        let out = sender.submit(RequestFromKernel::new(
            twizzler_abi::pager::KernelCommand::EchoReq,
        ));
        logln!("kernel: submitted request");
        let resp = out.wait();
        logln!("got response: {:?}", resp);
        current_thread_ref()
            .unwrap()
            .set_state(ExecutionState::Sleeping);
        logln!("kernel: got response: {:?}", resp);
        // TODO: enter normal pager operation...
        schedule(false);
    }
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
        let sender = ManagedQueueSender::new(queue);
        unsafe { PAGER_QUEUES.sender = Some(sender) };
    } else {
        let queue = QueueObject::<RequestFromPager, CompletionToPager>::from_object(obj);
        let receiver = ManagedQueueReceiver::new(queue);
        unsafe { PAGER_QUEUES.receiver = Some(receiver) };
    }
    if unsafe { PAGER_QUEUES.receiver.is_some() && PAGER_QUEUES.sender.is_some() } {
        start_new_kernel(Priority::default_user(), pager_entry, 0);
        start_new_kernel(Priority::default_user(), pager_compl_handler_entry, 0);
        start_new_kernel(Priority::default_user(), pager_request_handler_entry, 0);
    }
}
