use std::{
    cell::UnsafeCell,
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Condvar, Mutex,
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
            for _ in 0..100 {
                let req = self.req.poll(self);
                if req.is_ok() {
                    return req;
                }
                let kind = req.as_ref().unwrap_err().kind();
                if kind != ErrorKind::WouldBlock {
                    return req;
                }
            }

            sys_thread_sync(&mut [ThreadSync::new_sleep(wait)], None)?;
        }
    }
}

unsafe impl Send for NvmeRequester {}
unsafe impl Sync for NvmeRequester {}

const READY: u64 = 1;
const DROPPED: u64 = 2;

pub struct NvmeRequest {
    cmd: CommonCommand,
    ready: UnsafeCell<MaybeUninit<CommonCompletion>>,
    flags: AtomicU64,
}

impl<'a> Drop for InflightRequest<'a> {
    fn drop(&mut self) {
        let requests = &mut self.req.inner.lock().unwrap().requests;
        let entry = requests.get(self.id as usize).unwrap();
        if entry.flags.fetch_or(DROPPED, Ordering::SeqCst) & READY != 0 {
            requests.remove(self.id as usize);
        }
    }
}

impl<'a> TwizzlerWaitable for InflightRequest<'a> {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let requests = &self.req.inner.lock().unwrap().requests;
        let req = requests.get(self.id as usize).unwrap();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&req.flags),
            0,
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
    pub fn get_completion(&mut self) -> Option<CommonCompletion> {
        let Some((bell, resp)) = self.comq.get_completion::<CommonCompletion>() else {
            return None;
        };
        self.subq.update_head(resp.new_sq_head());
        self.com_bell().write(bell as u32);
        let id: u16 = resp.command_id().into();
        let entry = self.requests.get(id as usize).unwrap();
        unsafe { entry.ready.get().as_mut().unwrap().write(resp) };
        if entry.flags.fetch_or(READY, Ordering::SeqCst) & DROPPED != 0 {
            self.requests.remove(id as usize);
        } else {
            let _ = twizzler_abi::syscall::sys_thread_sync(
                &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                    ThreadSyncReference::Virtual(&entry.flags),
                    usize::MAX,
                ))],
                None,
            );
        }

        Some(resp)
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
            self.requests.remove(id as usize);
            None
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

    pub fn poll(&self, inflight: &InflightRequest) -> std::io::Result<CommonCompletion> {
        self.inner.lock().unwrap().poll(inflight)
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

    pub fn get_completion(&self) -> Option<CommonCompletion> {
        let cc = self.inner.lock().unwrap().get_completion();
        if cc.is_some() {
            self.cv.notify_one();
        }
        cc
    }
}
