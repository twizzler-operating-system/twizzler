use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use twizzler_abi::{
    device::CacheType,
    object::{NULLPAGE_SIZE, Protections},
    syscall::{
        MapFlags, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
        ThreadSyncWake,
    },
};
use twizzler_queue_raw::{
    QueueBase, QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags,
};

use crate::{
    condvar::CondVar,
    memory::context::{
        Context, KernelMemoryContext, KernelObjectHandle, ObjectContextInfo, kernel_context,
    },
    mutex::Mutex,
    obj::{ObjectRef, PageNumber},
    processor::spin_wait_until,
    spinlock::Spinlock,
    syscall::sync::sys_thread_sync,
};

/// Floor for the adaptive receive spin. Cheap enough to be worth keeping for the case the spin is
/// actually for -- a second entry already on its way -- while being ~1.5% of the length that costs
/// nothing but time when it is not.
const RECV_SPIN_MIN: usize = 16;
/// Ceiling: the raw queue's own default, so adapting can only ever spin less than not adapting.
const RECV_SPIN_MAX: usize = twizzler_queue_raw::SPIN_ATTEMPTS;

/// What a queue's receive side has been doing, for [`QueueObject::completion_recv_stats`].
pub struct QueueRecvStats {
    pub recvs: u64,
    pub parks: u64,
    pub spins: u64,
    pub budget: usize,
}

struct Queue<T> {
    raw: RawQueue<T>,
    cv: CondVar,
    lock: Spinlock<()>,
    /// Spin budget for [`Queue::recv`], halved when a spend ends in a park anyway and doubled when
    /// one wins, clamped to [RECV_SPIN_MIN]..=[RECV_SPIN_MAX].
    ///
    /// A fixed budget is only right when the producer's cadence is known, and the kernel's two
    /// pager queues sit at opposite ends of it: the request queue's producer is the pager
    /// answering whenever it feels like it, while a completion burst arrives back to back. The
    /// consumer cannot be told which it is facing, but it can watch: a budget spent in full
    /// and followed by a park is evidence it was too long, and a budget that ran out just
    /// short of an arrival is evidence it was too short. Multiplicative both ways so a
    /// workload that changes phase is tracked in a few receives rather than a few thousand.
    ///
    /// Only a spin that was actually *entered* votes. A receive that found an entry waiting spends
    /// nothing and learns nothing -- counting those as wins would ratchet the budget to the
    /// ceiling on exactly the streaming workload that never needs to spin at all.
    spin: AtomicUsize,
    recvs: AtomicU64,
    parks: AtomicU64,
    spins: AtomicU64,
}

unsafe impl<T: Copy> Send for Queue<T> {}
unsafe impl<T: Copy> Sync for Queue<T> {}

impl<T: Copy> Queue<T> {
    unsafe fn new(hdr: *const RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            raw: unsafe { RawQueue::new(hdr, buf) },
            cv: CondVar::new(),
            lock: Spinlock::new(()),
            spin: AtomicUsize::new(RECV_SPIN_MAX),
            recvs: AtomicU64::new(0),
            parks: AtomicU64::new(0),
            spins: AtomicU64::new(0),
        }
    }

    fn stats(&self) -> QueueRecvStats {
        QueueRecvStats {
            recvs: self.recvs.load(Ordering::Relaxed),
            parks: self.parks.load(Ordering::Relaxed),
            spins: self.spins.load(Ordering::Relaxed),
            budget: self.spin.load(Ordering::Relaxed),
        }
    }

    fn send(&self, item: T, info: u32) {
        self.raw
            .submit(
                QueueEntry::new(info, item),
                |word, val| {
                    sys_thread_sync(
                        &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                            ThreadSyncReference::Virtual(word),
                            val,
                            ThreadSyncOp::Equal,
                            ThreadSyncFlags::empty(),
                        ))],
                        None,
                    )
                    .unwrap();
                },
                |word| {
                    sys_thread_sync(
                        &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                            ThreadSyncReference::Virtual(word),
                            usize::MAX,
                        ))],
                        None,
                    )
                    .unwrap();
                },
                SubmissionFlags::empty(),
            )
            .unwrap();
    }

    fn recv(&self) -> (u32, T) {
        let budget = self.spin.load(Ordering::Relaxed);
        // Set from inside the wait callback rather than inferred from `spun == budget`: the loop
        // re-arms and waits again on a spurious wake, and only the callback firing distinguishes
        // "ran out of spin and slept" from "ran out of spin and then found an entry".
        let parked = Cell::new(false);
        let (item, spun) = self
            .raw
            .receive_spin(
                budget,
                |word, val| {
                    parked.set(true);
                    sys_thread_sync(
                        &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                            ThreadSyncReference::Virtual(word),
                            val,
                            ThreadSyncOp::Equal,
                            ThreadSyncFlags::empty(),
                        ))],
                        None,
                    )
                    .unwrap();
                },
                |word| {
                    sys_thread_sync(
                        &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                            ThreadSyncReference::Virtual(word),
                            usize::MAX,
                        ))],
                        None,
                    )
                    .unwrap();
                },
                ReceiveFlags::empty(),
            )
            .unwrap();
        if spun != 0 {
            let next = if parked.get() {
                (budget / 2).max(RECV_SPIN_MIN)
            } else {
                (budget * 2).min(RECV_SPIN_MAX)
            };
            // Racy against another receiver by construction, and harmlessly so: `QueueObject`
            // admits one receiver at a time per direction, and a lost update costs one
            // mis-sized spin.
            if next != budget {
                self.spin.store(next, Ordering::Relaxed);
            }
        }
        self.recvs.fetch_add(1, Ordering::Relaxed);
        self.spins.fetch_add(spun as u64, Ordering::Relaxed);
        if parked.get() {
            self.parks.fetch_add(1, Ordering::Relaxed);
        }
        (item.info(), item.item())
    }
}

pub struct QueueObject<S, C> {
    handle: <Context as KernelMemoryContext>::Handle<QueueBase<S, C>>,
    submissions: Queue<S>,
    completions: Queue<C>,
    sguard: AtomicBool,
    cguard: AtomicBool,
}

impl<S: Copy, C: Copy> QueueObject<S, C> {
    pub fn from_object(obj: ObjectRef) -> Self {
        crate::memory::context::kobjcensus::record(crate::memory::context::kobjcensus::Site::Queue);
        let handle =
            kernel_context().insert_kernel_object::<QueueBase<S, C>>(ObjectContextInfo::new(
                obj.clone(),
                Protections::READ | Protections::WRITE,
                CacheType::WriteBack,
                MapFlags::empty(),
            ));
        let base = handle.base();

        let max_base = base
            .sub_buf
            .max(base.sub_hdr)
            .max(base.com_buf)
            .max(base.com_hdr);

        let sub = unsafe {
            Queue::new(
                handle.lea_raw(base.sub_hdr as *const RawQueueHdr).unwrap(),
                handle
                    .lea_raw_mut(base.sub_buf as *mut QueueEntry<S>)
                    .unwrap(),
            )
        };
        let com = unsafe {
            Queue::new(
                handle.lea_raw(base.com_hdr as *const RawQueueHdr).unwrap(),
                handle
                    .lea_raw_mut(base.com_buf as *mut QueueEntry<C>)
                    .unwrap(),
            )
        };
        let max_len = com
            .raw
            .hdr()
            .len_bytes()
            .max(sub.raw.hdr().len_bytes())
            .max(size_of::<RawQueueHdr>());
        let num_bytes = NULLPAGE_SIZE + max_base + max_len;
        log::debug!(
            "pre-faulting {} bytes ({} {}) {:?}",
            num_bytes,
            max_base,
            max_len,
            obj.id()
        );
        let mut pt = obj.lock_page_tables();
        for pg in 0..(num_bytes / PageNumber::PAGE_SIZE) {
            pt = obj
                .ensure_in_core(
                    pt,
                    PageNumber::from_offset(pg * PageNumber::PAGE_SIZE),
                    1,
                    &mut false,
                    &mut false,
                )
                .unwrap();
        }
        Self {
            handle,
            submissions: sub,
            completions: com,
            sguard: Default::default(),
            cguard: Default::default(),
        }
    }

    pub fn submit(&self, item: S, info: u32) {
        self.submissions.send(item, info)
    }

    pub fn complete(&self, item: C, info: u32) {
        self.completions.send(item, info)
    }

    pub fn recv(&self) -> (u32, S) {
        spin_wait_until(
            || {
                if self.sguard.swap(true, Ordering::SeqCst) {
                    None
                } else {
                    Some(())
                }
            },
            || {},
        );
        let r = self.submissions.recv();
        self.sguard.store(false, Ordering::SeqCst);
        r
    }

    /// Receive-side counters for the completion subqueue, which for the pager's sender is the one
    /// the completion thread blocks on.
    pub fn completion_recv_stats(&self) -> QueueRecvStats {
        self.completions.stats()
    }

    pub fn recv_completion(&self) -> (u32, C) {
        spin_wait_until(
            || {
                if self.cguard.swap(true, Ordering::SeqCst) {
                    None
                } else {
                    Some(())
                }
            },
            || {},
        );
        let r = self.completions.recv();
        self.cguard.store(false, Ordering::SeqCst);
        r
    }
}

pub struct Outstanding<C> {
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
    pub fn wait(&self) -> C {
        loop {
            let data = self.data.lock();
            if let Some(c) = &*data {
                return *c;
            }
            let _ = self.cv.wait(data);
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

    pub fn submit(&self, item: S) -> Arc<Outstanding<C>> {
        let id = self.alloc_id();
        let outstanding = Arc::new(Outstanding::default());
        self.outstanding.lock().insert(id, outstanding.clone());
        self.queue.submit(item, id);
        outstanding
    }

    pub fn process_completion(&self) {
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
    pub fn new(queue: QueueObject<S, C>) -> Self {
        Self { queue }
    }

    pub fn handle_request<F>(&self, f: F)
    where
        F: FnOnce(u32, S) -> C,
    {
        let (id, item) = self.queue.recv();
        let resp = f(id, item);
        self.queue.complete(resp, id);
    }
}
