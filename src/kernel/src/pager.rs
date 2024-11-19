use inflight::InflightManager;
use request::ReqKind;
use twizzler_abi::{
    object::ObjID,
    pager::{CompletionToKernel, CompletionToPager, RequestFromKernel, RequestFromPager},
    thread::ExecutionState,
};

use crate::{
    mutex::Mutex,
    obj::{lookup_object, LookupFlags, ObjectRef},
    once::Once,
    queue::{ManagedQueueReceiver, ManagedQueueSender, QueueObject},
    sched::schedule,
    syscall::sync::finish_blocking,
    thread::{current_thread_ref, entry::start_new_kernel, priority::Priority},
};

mod inflight;
mod queues;
mod request;

pub use inflight::Inflight;
pub use queues::init_pager_queue;
pub use request::Request;

/*
extern "C" fn pager_entry() {
    pager_main();
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
*/

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
