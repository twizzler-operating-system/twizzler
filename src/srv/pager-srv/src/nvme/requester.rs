use std::{
    cell::UnsafeCell,
    io::ErrorKind,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex, OnceLock,
    },
};

use nvme::{
    ds::queue::{comentry::CommonCompletion, subentry::CommonCommand},
    queue::{CompletionQueue, SubmissionQueue},
};
use slab::Slab;
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_driver::device::MmioObject;
use twizzler_futures::TwizzlerWaitable;
use volatile::VolatilePtr;

use super::dma::NvmeDmaSliceRegion;

pub struct NvmeRequester {
    subq: Mutex<SubmissionQueue>,
    comq: Mutex<CompletionQueue>,
    sub_bell: *mut u32,
    com_bell: *mut u32,
    requests: Mutex<Slab<NvmeRequest>>,
    _sub_dma: NvmeDmaSliceRegion<CommonCommand>,
    _com_dma: NvmeDmaSliceRegion<CommonCompletion>,
    _bar_obj: MmioObject,
}

pub struct InflightRequest<'a> {
    req: &'a NvmeRequester,
    pub id: u16,
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
        tracing::info!("drop ifr {}", self.id);
        let mut requests = self.req.requests.lock().unwrap();
        let entry = requests.get(self.id as usize).unwrap();
        if entry.flags.fetch_or(DROPPED, Ordering::SeqCst) & READY != 0 {
            tracing::info!("{} dropped while ready", self.id);
            requests.remove(self.id as usize);
        }
    }
}

impl<'a> TwizzlerWaitable for InflightRequest<'a> {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let requests = self.req.requests.lock().unwrap();
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

impl NvmeRequester {
    pub fn new(
        subq: Mutex<SubmissionQueue>,
        comq: Mutex<CompletionQueue>,
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
            requests: Mutex::new(Slab::new()),
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
    pub fn get_completion(&self) -> Option<CommonCompletion> {
        let mut comq = self.comq.lock().unwrap();
        let Some((bell, resp)) = comq.get_completion::<CommonCompletion>() else {
            return None;
        };
        self.subq.lock().unwrap().update_head(resp.new_sq_head());
        self.com_bell().write(bell as u32);
        let id: u16 = resp.command_id().into();
        tracing::info!("got {} as compl", id);
        let mut requests = self.requests.lock().unwrap();
        let entry = requests.get(id as usize).unwrap();
        unsafe { entry.ready.get().as_mut().unwrap().write(resp) };
        if entry.flags.fetch_or(READY, Ordering::SeqCst) & DROPPED != 0 {
            tracing::info!("{} already dropped", id);
            requests.remove(id as usize);
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
    pub fn submit(&self, mut cmd: CommonCommand) -> Option<InflightRequest<'_>> {
        tracing::info!("0");
        let mut requests = self.requests.lock().unwrap();
        let entry = requests.vacant_entry();
        let id = entry.key() as u16;
        tracing::info!("a: {}", id);
        cmd.set_cid(id.into());
        entry.insert(NvmeRequest::new(cmd));
        let entry = requests.get(id as usize)?;
        tracing::info!("b: {}", id);
        let mut sq = self.subq.lock().unwrap();
        if let Some(tail) = sq.submit(&entry.cmd) {
            self.sub_bell().write(tail as u32);
            tracing::info!("c: {}", id);
            Some(InflightRequest { req: self, id })
        } else {
            requests.remove(id as usize);
            tracing::info!("x: {}", id);
            None
        }
    }

    pub fn poll(&self, inflight: &InflightRequest) -> std::io::Result<CommonCompletion> {
        tracing::info!("drop ifr {}", inflight.id);
        let requests = self.requests.lock().unwrap();
        let Some(entry) = requests.get(inflight.id as usize) else {
            return Err(ErrorKind::Other.into());
        };
        if entry.flags.load(Ordering::SeqCst) & READY != 0 {
            Ok(unsafe { entry.ready.get().as_ref().unwrap().assume_init_read() })
        } else {
            Err(ErrorKind::WouldBlock.into())
        }
    }
}
