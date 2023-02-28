use core::{cell::RefCell, sync::atomic::Ordering};

use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use twizzler_abi::{device::CacheType, object::Protections};
use twizzler_queue_raw::{
    QueueBase, QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags,
};

use crate::{
    condvar::CondVar,
    memory::context::{
        kernel_context, Context, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo,
    },
    mutex::Mutex,
    obj::{copy, ObjectRef},
    spinlock::Spinlock,
    thread::{start_new_kernel, Priority, ThreadRef},
};

struct Queue<T> {
    raw: RawQueue<T>,
    cv: CondVar,
    lock: Spinlock<()>,
}

impl<T: Copy> Queue<T> {
    unsafe fn new(hdr: *const RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            raw: RawQueue::new(hdr, buf),
            cv: CondVar::new(),
            lock: Spinlock::new(()),
        }
    }

    fn send(&self, item: T, info: u32) {
        self.raw
            .submit(
                QueueEntry::new(info, item),
                |word, val| {
                    let guard = self.lock.lock();
                    if word.load(Ordering::SeqCst) == val {
                        self.cv.wait(guard);
                    }
                },
                |_word| self.cv.signal(),
                SubmissionFlags::empty(),
            )
            .unwrap();
    }

    fn recv(&mut self) -> (u32, T) {
        let item = self
            .raw
            .receive(
                |word, val| {
                    let guard = self.lock.lock();
                    if word.load(Ordering::SeqCst) == val {
                        self.cv.wait(guard);
                    }
                },
                |_word| self.cv.signal(),
                ReceiveFlags::empty(),
            )
            .unwrap();
        (item.info(), item.item())
    }
}

pub struct QueueObject<S, C> {
    handle: <Context as KernelMemoryContext>::Handle<QueueBase<S, C>>,
    submissions: RefCell<Queue<S>>,
    completions: RefCell<Queue<C>>,
}

impl<S: Copy, C: Copy> QueueObject<S, C> {
    pub fn from_object(obj: ObjectRef) -> Self {
        let handle =
            kernel_context().insert_kernel_object::<QueueBase<S, C>>(ObjectContextInfo::new(
                obj,
                Protections::READ | Protections::WRITE,
                CacheType::WriteBack,
            ));
        let base = handle.base();
        let sub = unsafe {
            Queue::new(
                handle.lea_raw(base.sub_hdr as *const RawQueueHdr).unwrap(),
                handle
                    .lea_raw_mut(base.sub_hdr as *mut QueueEntry<S>)
                    .unwrap(),
            )
        };
        let com = unsafe {
            Queue::new(
                handle.lea_raw(base.com_hdr as *const RawQueueHdr).unwrap(),
                handle
                    .lea_raw_mut(base.com_hdr as *mut QueueEntry<C>)
                    .unwrap(),
            )
        };
        Self {
            handle,
            submissions: RefCell::new(sub),
            completions: RefCell::new(com),
        }
    }

    pub fn submit(&self, item: S, info: u32) {
        self.submissions.borrow().send(item, info)
    }

    pub fn complete(&self, item: C, info: u32) {
        self.completions.borrow().send(item, info)
    }

    pub fn recv(&self) -> (u32, S) {
        self.submissions.borrow_mut().recv()
    }

    pub fn recv_completion(&self) -> (u32, C) {
        self.completions.borrow_mut().recv()
    }
}

struct Outstanding<C> {
    data: Spinlock<Option<C>>,
    cv: CondVar,
}

impl<C> Default for Outstanding<C> {
    fn default() -> Self {
        Self {
            data: Spinlock::new(Default::default()),
            cv: CondVar::new(),
        }
    }
}

impl<C: Copy> Outstanding<C> {
    fn wait(&self) -> C {
        let mut data = self.data.lock();
        loop {
            if let Some(c) = &*data {
                return *c;
            }
            data = self.cv.wait(data);
        }
    }

    fn set(&self, item: C) {
        *self.data.lock() = Some(item);
        self.cv.signal();
    }
}

pub struct ManagedQueueSender<S, C> {
    queue: QueueObject<S, C>,
    outstanding: Mutex<BTreeMap<u32, Arc<Outstanding<C>>>>,
    id_stack: Spinlock<(u32, Vec<u32>)>,
}

impl<S: Copy, C: Copy> ManagedQueueSender<S, C> {
    pub fn new(queue: QueueObject<S, C>) -> Self {
        Self {
            queue,
            outstanding: Mutex::default(),
            id_stack: Spinlock::new((0, Vec::new())),
        }
    }

    fn alloc_id(&self) -> u32 {
        let mut stack = self.id_stack.lock();
        stack.1.pop().unwrap_or_else(|| {
            let next = stack.0;
            stack.0 += 1;
            next
        })
    }

    fn release_id(&self, id: u32) {
        let mut stack = self.id_stack.lock();
        stack.1.push(id);
    }

    fn submit(&self, item: S) -> Arc<Outstanding<C>> {
        let id = self.alloc_id();
        let outstanding = Arc::new(Outstanding::default());
        self.outstanding.lock().insert(id, outstanding.clone());
        self.queue.submit(item, id);
        outstanding
    }

    fn process_completion(&self) {
        let (id, item) = self.queue.recv_completion();
        let mut outstanding = self.outstanding.lock();
        if let Some(out) = outstanding.remove(&id) {
            out.set(item);
        }
        self.release_id(id);
    }
}

pub struct ManagedQueueReceiver<S, C> {
    queue: QueueObject<S, C>,
}

impl<S: Copy, C: Copy> ManagedQueueReceiver<S, C> {
    fn handle_request<F>(&self, f: F)
    where
        F: FnOnce(u32, S) -> C,
    {
        let (id, item) = self.queue.recv();
        let resp = f(id, item);
        self.queue.complete(item, id);
    }
}
