use std::{
    cell::UnsafeCell,
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Condvar, Mutex,
    },
    task::{Context, Poll, Waker},
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
            sys_thread_sync(&mut [ThreadSync::new_sleep(wait.0)], None)?;
        }
    }

    pub fn poll_completion(&self, cx: &mut Context<'_>) -> Poll<std::io::Result<CommonCompletion>> {
        if let Some(cc) = self.completion() {
            return Poll::Ready(Ok(cc));
        }
        // Only spin on the first poll; on a re-poll the waker is already registered.
        if self.entry.flags.load(Ordering::Acquire) & WAKER == 0 {
            if let Some(cc) = self.spin() {
                return Poll::Ready(Ok(cc));
            }
        }

        // Store the waker before advertising it, for the same reason as WAITER above.
        *self.entry.waker.lock().unwrap() = Some(cx.waker().clone());
        if self.entry.flags.fetch_or(WAKER, Ordering::SeqCst) & READY != 0 {
            return Poll::Ready(Ok(unsafe {
                self.entry.ready.get().as_ref().unwrap().assume_init_read()
            }));
        }
        Poll::Pending
    }
}

unsafe impl Send for NvmeRequester {}
unsafe impl Sync for NvmeRequester {}

const READY: u64 = 1;
const DROPPED: u64 = 2;
const WAITER: u64 = 4;
const WAKER: u64 = 8;

pub struct NvmeRequest {
    cmd: CommonCommand,
    ready: UnsafeCell<MaybeUninit<CommonCompletion>>,
    flags: AtomicU64,
    waker: Mutex<Option<Waker>>,
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
            waker: Mutex::new(None),
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
        if flags & DROPPED != 0 {
            tracing::info!("removing request {} due completion", id);
            self.requests.remove(id as usize);
        } else if flags & WAITER != 0 {
            let _ = twizzler_abi::syscall::sys_thread_sync(
                &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                    ThreadSyncReference::Virtual(&entry.flags),
                    usize::MAX,
                ))],
                None,
            );
        } else if flags & WAKER != 0 {
            let mut w = entry.waker.lock().unwrap();
            if let Some(waker) = w.take() {
                waker.wake();
            }
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
        }
    }

    #[inline]
    pub fn submit(&self, cmd: CommonCommand) -> Option<InflightRequest<'_>> {
        let (id, entry) = self.inner.lock().unwrap().submit(cmd)?;
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
                return Some(InflightRequest {
                    req: self,
                    id,
                    entry,
                });
            }
            if let Some(timeout) = timeout {
                let (guard, to) = self.cv.wait_timeout(inner, timeout).unwrap();
                if to.timed_out() {
                    return None;
                }
                inner = guard;
            } else {
                inner = self.cv.wait(inner).unwrap();
            }
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
        }
        if more {
            self.cv.notify_all();
        }
        more
    }

    pub fn get_completion(&self) -> Option<(u16, CommonCompletion)> {
        let cc = self.inner.lock().unwrap().get_completion();
        if cc.is_some() {
            self.cv.notify_one();
        }
        cc
    }
}
