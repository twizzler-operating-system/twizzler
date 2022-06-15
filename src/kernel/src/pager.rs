use core::sync::atomic::{AtomicBool, Ordering};

use alloc::{
    collections::{BTreeMap, VecDeque},
    vec::Vec,
};
use twizzler_abi::{
    object::ObjID,
    pager::{KernelCompletion, KernelRequest},
};
use twizzler_queue_raw::{ReceiveFlags, SubmissionFlags};

use crate::{
    condvar::CondVar,
    mutex::Mutex,
    queue::Queue,
    sched::{schedule, schedule_thread},
    spinlock::Spinlock,
    thread::{self, current_thread_ref, Priority, ThreadNewVMKind, ThreadRef},
};

static PAGER_READY: AtomicBool = AtomicBool::new(false);

static KERNEL_QUEUE: Queue<KernelRequest, KernelCompletion, PagerReqKey> = todo!();

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct PagerReqKey {
    id: ObjID,
    pagenr: usize,
}

struct Waiters {
    map: Mutex<BTreeMap<PagerReqKey, Vec<ThreadRef>>>,
}

static WAITERS: Waiters = todo!();

struct InternalQueue {
    queue: Spinlock<VecDeque<(PagerReqKey, KernelRequest)>>,
    cv: CondVar,
}

static INTQ: InternalQueue = todo!();

pub extern "C" fn pager_completion_thread_main() {
    thread::start_new_thread(
        thread::ThreadNewKind::Kernel(Priority::REALTIME, ThreadNewVMKind::Current),
        None,
        pager_submitter_thread_main,
    );
    loop {
        KERNEL_QUEUE.process_completions(false, ReceiveFlags::empty());
        logln!("pager completion thread exited processing loop");
    }
}

pub extern "C" fn pager_submitter_thread_main() {
    loop {
        let mut q = INTQ.queue.lock();
        while let Some(item) = q.pop_front() {
            KERNEL_QUEUE
                .submit(item.1, item.0, handle_completion, SubmissionFlags::empty())
                .unwrap();
        }
        INTQ.cv.wait(q);
    }
}

fn handle_completion(key: PagerReqKey, cmp: KernelCompletion) {
    let mut waiters = WAITERS.map.lock();
    if let Some(mut list) = waiters.remove(&key) {
        while let Some(entry) = list.pop() {
            schedule_thread(entry);
        }
    }
}

fn submit_pager_request(key: PagerReqKey, req: KernelRequest) {
    if !PAGER_READY.load(Ordering::SeqCst) {
        panic!("tried to submit a paging request before pager initialized");
    }
    let existing = {
        let mut waiters = WAITERS.map.lock();
        let existing = waiters.contains_key(&key);
        if !existing {
            waiters.insert(key, Vec::new());
        }
        let list = waiters.get_mut(&key).unwrap();
        list.push(current_thread_ref().unwrap());
        existing
    };
    if !existing {
        let mut q = INTQ.queue.lock();
        q.push_back((key, req));
        INTQ.cv.signal();
    }
    schedule(false);
}

pub fn init_pager(kq: ObjID, pq: ObjID) {
    logln!("kernel has kq and pq {} {}", kq, pq);
    thread::start_new_thread(
        thread::ThreadNewKind::Kernel(Priority::REALTIME, ThreadNewVMKind::Blank),
        None,
        pager_completion_thread_main,
    );
    PAGER_READY.store(true, Ordering::SeqCst);
}
