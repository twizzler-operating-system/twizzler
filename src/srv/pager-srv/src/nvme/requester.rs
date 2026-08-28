use std::{
    cell::UnsafeCell,
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

use nvme::{
    ds::queue::{comentry::CommonCompletion, subentry::CommonCommand},
    queue::{CompletionQueue, SubmissionQueue},
};
use slab::Slab;
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep,
    ThreadSyncWake,
};
use twizzler_driver::device::MmioObject;
use twizzler_futures::TwizzlerWaitable;
use volatile::VolatilePtr;

use super::dma::NvmeDmaSliceRegion;

pub struct NvmeRequesterInner {
    subq: SubmissionQueue,
    comq: CompletionQueue,
    sub_bell: *mut u32,
    com_bell: *mut u32,
    requests: Slab<Arc<NvmeRequest>>,
    _sub_dma: NvmeDmaSliceRegion<CommonCommand>,
    _com_dma: NvmeDmaSliceRegion<CommonCompletion>,
    _bar_obj: MmioObject,
}

pub struct NvmeRequester {
    inner: Mutex<NvmeRequesterInner>,
    cv: Condvar,
    /// Diagnostic only. Counted outside the lock so a dump can report them even when the lock is
    /// the thing that is stuck.
    submitted: AtomicU64,
    completed: AtomicU64,
}

pub struct InflightRequest<'a> {
    pub req: &'a NvmeRequester,
    pub id: u16,
    /// Held directly rather than looked up in the slab per poll: waiting must not touch the
    /// requester lock, and the slab moves its entries as it grows.
    entry: Arc<NvmeRequest>,
}

/// Iterations to spin on the completion flag before parking. Completions are reaped by the
/// interrupt thread, so this only has to cover the latency of an already-fast device.
const SPIN_ITERS: usize = 4096;
/// How often, within a spin, to try reaping completions ourselves in case the interrupt thread is
/// descheduled. Uses `try_lock`, so a spinning waiter never blocks a submitter.
const SPIN_DRAIN_EVERY: usize = 256;

impl<'a> InflightRequest<'a> {
    fn completion(&self) -> Option<CommonCompletion> {
        if self.entry.flags.load(Ordering::Acquire) & READY != 0 {
            Some(unsafe { self.entry.ready.get().as_ref().unwrap().assume_init_read() })
        } else {
            None
        }
    }

    /// Spin on the completion flag, holding no lock, occasionally draining the completion queue.
    fn spin(&self) -> Option<CommonCompletion> {
        for i in 0..SPIN_ITERS {
            if let Some(cc) = self.completion() {
                return Some(cc);
            }
            if i % SPIN_DRAIN_EVERY == 0 {
                self.req.try_check_completions();
            }
            core::hint::spin_loop();
        }
        self.completion()
    }

    pub fn _poll(&self) -> std::io::Result<CommonCompletion> {
        self.completion().ok_or(ErrorKind::WouldBlock.into())
    }

    pub fn wait(&self) -> std::io::Result<CommonCompletion> {
        loop {
            let wait = self.wait_item_read();
            if let Some(cc) = self.spin() {
                return Ok(cc);
            }

            // Publish WAITER before the final check: `get_completion` sends its wake after setting
            // READY, so whichever of the two stores lands second observes the other's bit.
            self.entry.flags.fetch_or(WAITER, Ordering::SeqCst);
            if let Some(cc) = self.completion() {
                return Ok(cc);
            }
            // Timed, not indefinite. Data queues have no reaper thread -- they are drained by
            // whoever is waiting on them -- and this path does not know its queue's interrupt
            // word, so it re-checks rather than trusting someone else to wake it. `wait_owned` is
            // the one that sleeps on the interrupt properly; this is the admin queue and the odd
            // caller that submitted from another thread.
            sys_thread_sync(
                &mut [ThreadSync::new_sleep(wait.0)],
                Some(Duration::from_micros(200)),
            )
            .ok();
            self.req.try_check_completions();
        }
    }

    /// Blocking wait that reaps its own queue, for data-queue commands.
    ///
    /// Sleeps on this request's flags word *and* the calling thread's queue interrupt in one
    /// `sys_thread_sync`. That is sound because submission and waiting now happen on the same
    /// thread, so "the calling thread's queue" is the queue this command went out on. Contrast
    /// `wait`, which does not know its queue and therefore polls on a bound.
    pub fn wait_owned(&self) -> std::io::Result<CommonCompletion> {
        loop {
            if let Some(cc) = self.spin() {
                return Ok(cc);
            }
            // Must consume the interrupt word before arming: `spin` drains the completion queue but
            // leaves the word set, and `setup_interrupt_sleep` blocks only while it is zero, so
            // arming without consuming turns the sleep below into a spin.
            crate::nvme::reap_current_queue();

            // Publish WAITER before the final check: `get_completion` sends its wake after setting
            // READY, so whichever of the two stores lands second observes the other's bit.
            self.entry.flags.fetch_or(WAITER, Ordering::SeqCst);
            if let Some(cc) = self.completion() {
                return Ok(cc);
            }

            let mut ops = heapless::Vec::<ThreadSync, 2>::new();
            let _ = ops.push(ThreadSync::new_sleep(self.wait_item_read().0));
            if let Some(int) = crate::nvme::current_queue_sleep() {
                let _ = ops.push(ThreadSync::new_sleep(int));
                // The parks/ints diagnostic used to be stamped by `threads::park_poll`, which was
                // then the only place a data queue was waited on. This is that place now, so the
                // counter moves with it -- left behind it would have read 0 forever and quietly
                // stopped being able to report the state it exists to catch (parks with no
                // interrupts = completions arriving only via the spin drain).
                crate::nvme::controller::note_park();
            }
            sys_thread_sync(&mut ops, None).ok();
            crate::nvme::reap_current_queue();
        }
    }
}

unsafe impl Send for NvmeRequester {}
unsafe impl Sync for NvmeRequester {}

const READY: u64 = 1;
const DROPPED: u64 = 2;
const WAITER: u64 = 4;

pub struct NvmeRequest {
    cmd: CommonCommand,
    ready: UnsafeCell<MaybeUninit<CommonCompletion>>,
    flags: AtomicU64,
}

impl<'a> Drop for InflightRequest<'a> {
    #[track_caller]
    fn drop(&mut self) {
        // Whichever of this and `get_completion` observes the other's bit frees the slab slot, so
        // exactly one of them does. The `Arc` keeps the entry itself alive either way.
        if self.entry.flags.fetch_or(DROPPED, Ordering::SeqCst) & READY != 0 {
            self.req
                .inner
                .lock()
                .unwrap()
                .requests
                .remove(self.id as usize);
        } else {
            tracing::warn!(
                "drop inflight request {} while not ready: {}",
                self.id,
                core::panic::Location::caller()
            );
        }
    }
}

impl<'a> TwizzlerWaitable for InflightRequest<'a> {
    fn wait_item_read(&self) -> (twizzler_abi::syscall::ThreadSyncSleep, bool) {
        (
            ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&self.entry.flags),
                WAITER,
                twizzler_abi::syscall::ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ),
            false,
        )
    }

    fn wait_item_write(&self) -> (twizzler_abi::syscall::ThreadSyncSleep, bool) {
        self.wait_item_read()
    }
}

impl NvmeRequest {
    pub fn new(cmd: CommonCommand) -> Self {
        Self {
            cmd,
            ready: UnsafeCell::new(MaybeUninit::uninit()),
            flags: AtomicU64::new(0),
        }
    }
}

impl NvmeRequesterInner {
    pub fn new(
        subq: SubmissionQueue,
        comq: CompletionQueue,
        sub_bell: *mut u32,
        com_bell: *mut u32,
        bar_obj: MmioObject,
        sub_dma: NvmeDmaSliceRegion<CommonCommand>,
        com_dma: NvmeDmaSliceRegion<CommonCompletion>,
    ) -> Self {
        Self {
            subq,
            comq,
            sub_bell,
            com_bell,
            requests: Slab::new(),
            _sub_dma: sub_dma,
            _com_dma: com_dma,
            _bar_obj: bar_obj,
        }
    }

    #[inline]
    fn sub_bell(&self) -> VolatilePtr<'_, u32> {
        unsafe { VolatilePtr::new(NonNull::new(self.sub_bell).unwrap()) }
    }

    #[inline]
    fn com_bell(&self) -> VolatilePtr<'_, u32> {
        unsafe { VolatilePtr::new(NonNull::new(self.com_bell).unwrap()) }
    }

    #[inline]
    pub fn get_completion(&mut self) -> Option<(u16, CommonCompletion)> {
        let Some((bell, resp)) = self.comq.get_completion::<CommonCompletion>() else {
            return None;
        };
        self.subq.update_head(resp.new_sq_head());
        self.com_bell().write(bell as u32);
        let id: u16 = resp.command_id().into();
        let entry = self.requests.get(id as usize).unwrap().clone();
        unsafe { entry.ready.get().as_mut().unwrap().write(resp) };
        let flags = entry.flags.fetch_or(READY, Ordering::SeqCst);
        // Each bit is answered independently: these are not mutually exclusive states, and a chain
        // of `else if`s silently drops the second wake for an entry carrying two of them.
        if flags & WAITER != 0 {
            let _ = twizzler_abi::syscall::sys_thread_sync(
                &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                    ThreadSyncReference::Virtual(&entry.flags),
                    usize::MAX,
                ))],
                None,
            );
        }
        if flags & DROPPED != 0 {
            tracing::info!("removing request {} due completion", id);
            self.requests.remove(id as usize);
        }

        Some((id, resp))
    }

    #[inline]
    pub fn submit(&mut self, mut cmd: CommonCommand) -> Option<(u16, Arc<NvmeRequest>)> {
        let entry = self.requests.vacant_entry();
        let id = entry.key() as u16;
        cmd.set_cid(id.into());
        let req = entry.insert(Arc::new(NvmeRequest::new(cmd))).clone();
        if let Some(tail) = self.subq.submit(&req.cmd) {
            self.sub_bell().write(tail as u32);
            Some((id, req))
        } else {
            tracing::info!("removing request {} due overflow", id);
            self.requests.remove(id as usize);
            None
        }
    }
}

impl NvmeRequester {
    pub fn new(
        subq: SubmissionQueue,
        comq: CompletionQueue,
        sub_bell: *mut u32,
        com_bell: *mut u32,
        bar_obj: MmioObject,
        sub_dma: NvmeDmaSliceRegion<CommonCommand>,
        com_dma: NvmeDmaSliceRegion<CommonCompletion>,
    ) -> Self {
        Self {
            inner: Mutex::new(NvmeRequesterInner::new(
                subq, comq, sub_bell, com_bell, bar_obj, sub_dma, com_dma,
            )),
            cv: Condvar::new(),
            submitted: AtomicU64::new(0),
            completed: AtomicU64::new(0),
        }
    }

    /// Dump enough state to tell a lost completion from a lost wakeup.
    ///
    /// `try_lock`, never `lock`: this is called from the watchdog while the pager is wedged, and a
    /// dump that blocks on the very lock under investigation reports nothing. A contended lock is
    /// itself the answer, so it is printed rather than waited on.
    pub fn dump(&self, label: &str) {
        let submitted = self.submitted.load(Ordering::Relaxed);
        let completed = self.completed.load(Ordering::Relaxed);
        let Ok(inner) = self.inner.try_lock() else {
            tracing::warn!(
                "nvme dump {}: LOCK HELD, submitted {} completed {}",
                label,
                submitted,
                completed
            );
            return;
        };
        // `cq_ready` is the discriminator: true means the device posted a completion that nobody
        // has consumed, so the bug is on this side of the wire.
        tracing::warn!(
            "nvme dump {}: submitted {} completed {} outstanding {} live-slots {} sq_full {} sq_empty {} cq_ready {}",
            label,
            submitted,
            completed,
            submitted.wrapping_sub(completed),
            inner.requests.len(),
            inner.subq.is_full(),
            inner.subq.is_empty(),
            inner.comq.ready(),
        );
        for (id, entry) in inner.requests.iter().take(24) {
            tracing::warn!(
                "nvme dump {}:   req {} flags {:#x}",
                label,
                id,
                entry.flags.load(Ordering::Relaxed),
            );
        }
    }

    #[inline]
    pub fn submit(&self, cmd: CommonCommand) -> Option<InflightRequest<'_>> {
        let (id, entry) = self.inner.lock().unwrap().submit(cmd)?;
        self.submitted.fetch_add(1, Ordering::Relaxed);
        Some(InflightRequest {
            req: self,
            id,
            entry,
        })
    }

    #[inline]
    pub fn submit_wait(
        &self,
        cmd: CommonCommand,
        timeout: Option<Duration>,
    ) -> Option<InflightRequest<'_>> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some((id, entry)) = inner.submit(cmd) {
                self.submitted.fetch_add(1, Ordering::Relaxed);
                return Some(InflightRequest {
                    req: self,
                    id,
                    entry,
                });
            }
            // The queue is full and we may be the only thread that can drain it, so drain here
            // rather than waiting to be notified -- with no per-queue reaper thread, waiting on the
            // condvar alone would be a deadlock whenever this thread owns the queue.
            let mut drained = false;
            while inner.get_completion().is_some() {
                drained = true;
                self.completed.fetch_add(1, Ordering::Relaxed);
            }
            if drained {
                continue;
            }
            // Nothing to drain: the device still owes us. Bounded so we come back and re-drain;
            // this path needs 64 commands outstanding from one thread, so it is close to
            // unreachable at `PIPELINE_DEPTH`.
            let (guard, to) = self
                .cv
                .wait_timeout(inner, timeout.unwrap_or(Duration::from_micros(200)))
                .unwrap();
            if to.timed_out() && timeout.is_some() {
                return None;
            }
            inner = guard;
        }
    }

    /// Reap completions, but only if nobody else holds the lock. Called from spin loops, where
    /// blocking on the lock is exactly what we are trying to avoid.
    fn try_check_completions(&self) -> bool {
        let Ok(mut inner) = self.inner.try_lock() else {
            return false;
        };
        let mut more = false;
        while let Some(_) = inner.get_completion() {
            more = true;
            self.completed.fetch_add(1, Ordering::Relaxed);
        }
        drop(inner);
        if more {
            self.cv.notify_all();
        }
        more
    }

    pub fn check_completions(&self) -> bool {
        let mut inner = self.inner.lock().unwrap();
        let mut more = false;
        while let Some(_) = inner.get_completion() {
            more = true;
            self.completed.fetch_add(1, Ordering::Relaxed);
        }
        if more {
            self.cv.notify_all();
        }
        more
    }

    pub fn get_completion(&self) -> Option<(u16, CommonCompletion)> {
        let cc = self.inner.lock().unwrap().get_completion();
        if cc.is_some() {
            self.completed.fetch_add(1, Ordering::Relaxed);
            self.cv.notify_one();
        }
        cc
    }
}
