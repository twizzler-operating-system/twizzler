//! Manage events for a device, including mailbox messages and interrupts.

use std::{
    collections::VecDeque,
    io::{Error, ErrorKind},
    pin::Pin,
    sync::{atomic::Ordering, Arc, Mutex},
};

use async_io::Async;
use futures::future::select_all;
use twizzler_abi::{
    device::{
        BusType, DeviceInterruptFlags, DeviceRepr, InterruptVector, MailboxPriority,
        NUM_DEVICE_INTERRUPTS,
    },
    kso::KactionError,
    syscall::{ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep},
};
use twizzler_futures::TwizzlerWaitable;

use super::Device;

struct DeviceEventStreamInner {
    msg_queue: Vec<VecDeque<u64>>,
}

impl DeviceEventStreamInner {
    fn new() -> Self {
        Self {
            msg_queue: (0..(MailboxPriority::Num as usize))
                .into_iter()
                .map(|_| VecDeque::new())
                .collect(),
        }
    }
}

struct IntInner {
    inum: usize,
    repr: Arc<Device>,
}

impl IntInner {
    fn repr(&self) -> &DeviceRepr {
        self.repr.repr()
    }

    fn new(repr: Arc<Device>, inum: usize) -> Self {
        Self { inum, repr }
    }
}

impl TwizzlerWaitable for IntInner {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let repr = self.repr();
        repr.setup_interrupt_sleep(self.inum)
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let repr = self.repr();
        repr.setup_interrupt_sleep(self.inum)
    }
}

struct MailboxInner {
    repr: Arc<Device>,
    inum: usize,
}

impl Unpin for MailboxInner {}
impl Unpin for IntInner {}

impl MailboxInner {
    fn repr(&self) -> &DeviceRepr {
        self.repr.repr()
    }

    fn new(repr: Arc<Device>, inum: usize) -> Self {
        Self { inum, repr }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Possible errors for interrupt allocation.
pub enum InterruptAllocationError {
    /// The device has run out of interrupt vectors that can be used.
    NoMoreInterrupts,
    /// Some option was unsupported.
    Unsupported,
    /// The kernel encountered an error.
    KernelError(KactionError),
}

impl TwizzlerWaitable for MailboxInner {
    fn wait_item_read(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.repr().mailboxes[self.inum]),
            0,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    fn wait_item_write(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.repr().mailboxes[self.inum]),
            0,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

/// A manager for device events, including interrupt handling.
pub struct DeviceEventStream {
    inner: Mutex<DeviceEventStreamInner>,
    asyncs: Vec<Async<Pin<Box<IntInner>>>>,
    async_mb: Vec<Async<Pin<Box<MailboxInner>>>>,
    device: Arc<Device>,
}

/// A handle for an allocated interrupt on a device.
pub struct InterruptInfo {
    es: Arc<DeviceEventStream>,
    _vec: InterruptVector,
    devint: u32,
    inum: usize,
}

impl InterruptInfo {
    /// Wait until the next interrupt occurs.
    pub async fn next(&self) -> Option<u64> {
        self.es.next(self.inum).await
    }

    /// Get the interrupt number for programming the device.
    pub fn devint(&self) -> u32 {
        self.devint
    }
}

impl Drop for InterruptInfo {
    fn drop(&mut self) {
        self.es.free_interrupt(self)
    }
}

impl DeviceEventStream {
    pub(crate) fn free_interrupt(&self, _ii: &InterruptInfo) {
        // TODO
    }

    /// Allocate a new interrupt on this device.
    pub(crate) fn allocate_interrupt(
        self: &Arc<Self>,
    ) -> Result<InterruptInfo, InterruptAllocationError> {
        // SAFETY: We grab ownership of the interrupt repr data via the atomic swap.
        for i in 0..NUM_DEVICE_INTERRUPTS {
            if self.device.repr().interrupts[i]
                .taken
                .swap(1, std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                let (vec, devint) = match self.device.bus_type() {
                    BusType::Pcie => self.device.allocate_interrupt(i)?,
                    _ => return Err(InterruptAllocationError::Unsupported),
                };
                self.device
                    .repr_mut()
                    .register_interrupt(i, vec, DeviceInterruptFlags::empty());
                return Ok(InterruptInfo {
                    es: self.clone(),
                    _vec: vec,
                    devint,
                    inum: i,
                });
            }
        }
        Err(InterruptAllocationError::NoMoreInterrupts)
    }

    pub(crate) fn new(device: Arc<Device>) -> Self {
        let asyncs = (0..NUM_DEVICE_INTERRUPTS)
            .into_iter()
            .map(|i| Async::new(IntInner::new(device.clone(), i)).unwrap())
            .collect();
        let async_mb = (0..(MailboxPriority::Num as usize))
            .into_iter()
            .map(|i| Async::new(MailboxInner::new(device.clone(), i)).unwrap())
            .collect();
        Self {
            inner: Mutex::new(DeviceEventStreamInner::new()),
            asyncs,
            async_mb,
            device,
        }
    }

    fn repr(&self) -> &DeviceRepr {
        self.device.repr()
    }

    pub(crate) fn check_mailbox(&self, pri: MailboxPriority) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        inner.msg_queue[pri as usize].pop_front()
    }

    fn future_of_int(
        &self,
        inum: usize,
    ) -> impl std::future::Future<Output = Result<(usize, u64), Error>> + '_ {
        Box::pin(self.asyncs[inum].read_with(move |ii| {
            ii.repr()
                .check_for_interrupt(ii.inum)
                .ok_or(ErrorKind::WouldBlock.into())
                .map(|x| (inum, x))
        }))
    }

    fn future_of_mb(
        &self,
        inum: usize,
    ) -> impl std::future::Future<Output = Result<(usize, u64), Error>> + '_ {
        Box::pin(self.async_mb[inum].read_with(move |ii| {
            ii.repr()
                .check_for_mailbox(ii.inum)
                .ok_or(ErrorKind::WouldBlock.into())
                .map(|x| (inum, x))
        }))
    }

    fn check_add_msg(&self, i: usize) {
        if let Some(x) = self.repr().check_for_mailbox(i) {
            self.inner.lock().unwrap().msg_queue[i].push_back(x)
        }
    }

    pub(crate) async fn next(&self, int: usize) -> Option<u64> {
        if self.repr().interrupts[int].taken.load(Ordering::SeqCst) == 0 {
            return None;
        }
        if let Some(x) = self.repr().check_for_interrupt(int) {
            return Some(x);
        }

        let fut = self.future_of_int(int);
        fut.await.ok().map(|x| x.1)
    }

    pub(crate) async fn next_msg(&self, min: MailboxPriority) -> (MailboxPriority, u64) {
        loop {
            for i in 0..(MailboxPriority::Num as usize) {
                self.check_add_msg(i);
            }

            for i in ((min as usize)..(MailboxPriority::Num as usize)).rev() {
                if let Some(x) = self.check_mailbox(i.try_into().unwrap()) {
                    return (i.try_into().unwrap(), x);
                }
            }

            let futs = ((min as usize)..(MailboxPriority::Num as usize))
                .into_iter()
                .map(|i| self.future_of_mb(i));

            let (pri, x) = select_all(futs).await.0.unwrap();
            self.inner.lock().unwrap().msg_queue[pri].push_back(x);
        }
    }
}
