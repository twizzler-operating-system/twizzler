use std::{
    cell::UnsafeCell,
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Condvar, Mutex,
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
    requests: Slab<NvmeRequest>,
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
}

impl<'a> InflightRequest<'a> {
    pub fn poll(&self) -> std::io::Result<CommonCompletion> {
        self.req.poll(self)
    }

    pub fn wait(&self) -> std::io::Result<CommonCompletion> {
        loop {
            let wait = self.wait_item_read();
            let flags = self.req.get_flags(self);
            for _ in 0..100 {
                if unsafe { &*flags }.load(Ordering::Relaxed) & READY != 0 {
                    let req = self.req.poll(self);
                    if req.is_ok() {
                        return req;
                    }
                    let kind = req.as_ref().unwrap_err().kind();
                    if kind != ErrorKind::WouldBlock {
                        return req;
                    }
                }
            }

            let req = self.req.poll(self);
            if req.is_ok() {
                return req;
            }
            let kind = req.as_ref().unwrap_err().kind();
            if kind != ErrorKind::WouldBlock {
                return req;
            }

            unsafe { &*flags }.fetch_or(WAITER, Ordering::Release);
            sys_thread_sync(&mut [ThreadSync::new_sleep(wait)], None)?;
        }
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
        let requests = &mut self.req.inner.lock().unwrap().requests;
        let entry = requests.get(self.id as usize).unwrap();
        if entry.flags.fetch_or(DROPPED, Ordering::SeqCst) & READY != 0 {
            requests.remove(self.id as usize);
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
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let requests = &self.req.inner.lock().unwrap().requests;
        let req = requests.get(self.id as usize).unwrap();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&req.flags),
            WAITER,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
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
        let entry = self.requests.get(id as usize).unwrap();
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
    pub fn submit(&mut self, mut cmd: CommonCommand) -> Option<u16> {
        let entry = self.requests.vacant_entry();
        let id = entry.key() as u16;
        cmd.set_cid(id.into());
        entry.insert(NvmeRequest::new(cmd));
        let entry = self.requests.get(id as usize)?;
        if let Some(tail) = self.subq.submit(&entry.cmd) {
            self.sub_bell().write(tail as u32);
            Some(id)
        } else {
            tracing::info!("removing request {} due overflow", id);
            self.requests.remove(id as usize);
            None
        }
    }

    #[inline]
    pub fn async_poll(
        &mut self,
        inflight: &InflightRequest,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<CommonCompletion>> {
        let Some(mut entry) = self.requests.get(inflight.id as usize) else {
            tracing::warn!("no such request {}", inflight.id);
            return Poll::Ready(Err(ErrorKind::Other.into()));
        };
        let flags = entry.flags.load(Ordering::Acquire);
        if flags & READY != 0 {
            return Poll::Ready(Ok(unsafe {
                entry.ready.get().as_ref().unwrap().assume_init_read()
            }));
        }

        if flags & WAKER == 0 {
            for i in 0..30_000 {
                if entry.flags.load(Ordering::Acquire) & READY != 0 {
                    return Poll::Ready(Ok(unsafe {
                        entry.ready.get().as_ref().unwrap().assume_init_read()
                    }));
                }
                core::hint::spin_loop();
                if i % 4 == 0 {
                    if let Some((id, cc)) = self.get_completion() {
                        if id == inflight.id {
                            return Poll::Ready(Ok(cc));
                        }
                    }
                    entry = self.requests.get(inflight.id as usize).unwrap();
                }
            }
        }

        let Some(entry) = self.requests.get(inflight.id as usize) else {
            tracing::warn!("no such request (post-poll) {}", inflight.id);
            return Poll::Ready(Err(ErrorKind::Other.into()));
        };

        if entry.flags.fetch_or(WAKER, Ordering::SeqCst) & READY != 0 {
            Poll::Ready(Ok(unsafe {
                entry.ready.get().as_ref().unwrap().assume_init_read()
            }))
        } else {
            *entry.waker.lock().unwrap() = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    #[inline]
    pub fn poll(&self, inflight: &InflightRequest) -> std::io::Result<CommonCompletion> {
        let Some(entry) = self.requests.get(inflight.id as usize) else {
            return Err(ErrorKind::Other.into());
        };
        if entry.flags.load(Ordering::SeqCst) & READY != 0 {
            Ok(unsafe { entry.ready.get().as_ref().unwrap().assume_init_read() })
        } else {
            Err(ErrorKind::WouldBlock.into())
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
        let id = self.inner.lock().unwrap().submit(cmd)?;
        Some(InflightRequest { req: self, id })
    }

    #[inline]
    pub fn submit_wait(
        &self,
        cmd: CommonCommand,
        timeout: Option<Duration>,
    ) -> Option<InflightRequest<'_>> {
        let mut inner = self.inner.lock().unwrap();
        loop {
            if let Some(id) = inner.submit(cmd) {
                return Some(InflightRequest { req: self, id });
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

    pub fn async_poll(
        &self,
        inflight: &InflightRequest,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<CommonCompletion>> {
        self.inner.lock().unwrap().async_poll(inflight, cx)
    }

    pub fn poll(&self, inflight: &InflightRequest) -> std::io::Result<CommonCompletion> {
        self.inner.lock().unwrap().poll(inflight)
    }

    pub fn get_flags(&self, inflight: &InflightRequest) -> *const AtomicU64 {
        &self
            .inner
            .lock()
            .unwrap()
            .requests
            .get(inflight.id as usize)
            .unwrap()
            .flags
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
