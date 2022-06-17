use core::sync::atomic::{AtomicBool, Ordering};

use alloc::{
    collections::{BTreeMap, VecDeque},
    vec::Vec,
};
use twizzler_abi::{
    object::ObjID,
    pager::{KernelCompletion, KernelRequest, PagerCompletion, PagerRequest},
};
use twizzler_queue_raw::{ReceiveFlags, SubmissionFlags};

use crate::{
    condvar::CondVar,
    memory::context::MappingPerms,
    mutex::Mutex,
    obj::{lookup_object, LookupFlags},
    once::Once,
    operations::map_object_into_context,
    queue::Queue,
    sched::{schedule, schedule_thread},
    spinlock::Spinlock,
    thread::{
        self, current_memory_context, current_thread_ref, Priority, ThreadNewVMKind, ThreadRef,
    },
};

static PAGER_READY: AtomicBool = AtomicBool::new(false);

static KERNEL_QUEUE: Once<Queue<KernelRequest, KernelCompletion, Option<PagerReqKey>>> =
    Once::new();
static PAGER_QUEUE: Once<Queue<PagerRequest, PagerCompletion, ()>> = Once::new();

static QUEUE_IDS: Once<(ObjID, ObjID)> = Once::new();

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct PagerReqKey {
    id: ObjID,
    pagenr: usize,
}

struct Waiters {
    map: Mutex<BTreeMap<PagerReqKey, Vec<ThreadRef>>>,
}

static WAITERS: Once<Waiters> = Once::new();

struct InternalQueue {
    queue: Spinlock<VecDeque<(Option<PagerReqKey>, KernelRequest)>>,
    cv: CondVar,
}

static INTQ: Once<InternalQueue> = Once::new();

pub extern "C" fn pager_completion_thread_main() {
    let (kq, pq) = QUEUE_IDS.wait();
    let vm = current_memory_context().unwrap();
    let kqobj = lookup_object(*kq, LookupFlags::empty()).unwrap();
    let pqobj = lookup_object(*pq, LookupFlags::empty()).unwrap();
    map_object_into_context(0, kqobj, &vm, MappingPerms::READ | MappingPerms::WRITE).unwrap();
    map_object_into_context(1, pqobj, &vm, MappingPerms::READ | MappingPerms::WRITE).unwrap();
    unsafe {
        KERNEL_QUEUE.call_once(|| Queue::init_from_slots(*kq, 0));
        PAGER_QUEUE.call_once(|| Queue::init_from_slots(*pq, 1));
    }

    thread::start_new_thread(
        thread::ThreadNewKind::Kernel(Priority::REALTIME, ThreadNewVMKind::Current),
        None,
        pager_submitter_thread_main,
        Some("pager_subm"),
    );
    thread::start_new_thread(
        thread::ThreadNewKind::Kernel(Priority::REALTIME, ThreadNewVMKind::Current),
        None,
        pager_request_handler_thread,
        Some("pager_req"),
    );
    loop {
        KERNEL_QUEUE
            .wait()
            .process_completions(false, ReceiveFlags::empty());
        logln!("pager completion thread exited processing loop");
    }
}

pub extern "C" fn pager_submitter_thread_main() {
    let intq = INTQ.wait();
    loop {
        let mut q = intq.queue.lock();
        while let Some(item) = q.pop_front() {
            KERNEL_QUEUE
                .wait()
                .submit(item.1, item.0, handle_completion, SubmissionFlags::empty())
                .unwrap();
        }
        intq.cv.wait(q);
    }
}

pub extern "C" fn pager_request_handler_thread() {
    loop {
        PAGER_QUEUE.poll().unwrap().handle_reqs(|_req| {
            // logln!("pager request handler: got {:?}", req);
            PagerCompletion::Ok
        });
        logln!("pager request handler exited handling loop");
    }
}

fn handle_completion(key: Option<PagerReqKey>, _cmp: KernelCompletion) {
    //logln!("got completion {:?} for key {:?}", cmp, key);
    if let Some(key) = key {
        let mut waiters = WAITERS.wait().map.lock();
        if let Some(mut list) = waiters.remove(&key) {
            while let Some(entry) = list.pop() {
                schedule_thread(entry);
            }
        }
    }
}

fn submit_pager_request(key: Option<PagerReqKey>, req: KernelRequest) {
    //logln!("submitting req {:?}", req);
    if !PAGER_READY.load(Ordering::SeqCst) {
        panic!("tried to submit a paging request before pager initialized");
    }
    let intq = INTQ.wait();
    let existing = if let Some(key) = key {
        let mut waiters = WAITERS.wait().map.lock();
        let existing = waiters.contains_key(&key);
        if !existing {
            waiters.insert(key, Vec::new());
        }
        let list = waiters.get_mut(&key).unwrap();
        list.push(current_thread_ref().unwrap());
        existing
    } else {
        false
    };
    if !existing {
        let mut q = intq.queue.lock();
        q.push_back((key, req));
        intq.cv.signal();
    }
    if key.is_some() {
        schedule(false);
    }
}

pub fn init_pager(kq: ObjID, pq: ObjID) {
    logln!("[kernel::pager] setting up pager queues {} and {}", kq, pq);
    QUEUE_IDS.call_once(|| (kq, pq));
    WAITERS.call_once(|| Waiters {
        map: Mutex::new(BTreeMap::new()),
    });
    INTQ.call_once(|| InternalQueue {
        queue: Spinlock::new(VecDeque::new()),
        cv: CondVar::new(),
    });
    thread::start_new_thread(
        thread::ThreadNewKind::Kernel(Priority::REALTIME, ThreadNewVMKind::Blank),
        None,
        pager_completion_thread_main,
        Some("pager_compl"),
    );
    PAGER_READY.store(true, Ordering::SeqCst);
}
