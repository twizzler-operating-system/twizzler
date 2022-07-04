use std::{collections::VecDeque, sync::Mutex};

use futures::future::select_all;
use twizzler_abi::{
    device::{DeviceRepr, MailboxPriority, NUM_DEVICE_INTERRUPTS},
    syscall::{ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep},
};
use twizzler_async::{Async, AsyncSetup};

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
    repr: *const DeviceRepr,
}

impl IntInner {
    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
    }

    fn new(repr: *const DeviceRepr, inum: usize) -> Self {
        Self { inum, repr }
    }
}

impl AsyncSetup for IntInner {
    type Error = bool;

    const WOULD_BLOCK: Self::Error = true;

    fn setup_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        let repr = self.repr();
        repr.setup_interrupt_sleep(self.inum)
    }
}

struct MailboxInner {
    repr: *const DeviceRepr,
    inum: usize,
}

impl Unpin for MailboxInner {}
impl Unpin for IntInner {}

impl MailboxInner {
    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
    }

    fn new(repr: *const DeviceRepr, inum: usize) -> Self {
        Self { inum, repr }
    }
}

impl AsyncSetup for MailboxInner {
    type Error = bool;

    const WOULD_BLOCK: Self::Error = true;

    fn setup_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.repr().mailboxes[self.inum]),
            0,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }
}

pub struct DeviceEventStream {
    device: Device,
    inner: Mutex<DeviceEventStreamInner>,
    repr: *const DeviceRepr,
    asyncs: Vec<Async<IntInner>>,
    async_mb: Vec<Async<MailboxInner>>,
}

impl DeviceEventStream {
    pub fn new(device: Device) -> Self {
        let asyncs = (0..NUM_DEVICE_INTERRUPTS)
            .into_iter()
            .map(|i| Async::new(IntInner::new(device.repr(), i)))
            .collect();
        let async_mb = (0..(MailboxPriority::Num as usize))
            .into_iter()
            .map(|i| Async::new(MailboxInner::new(device.repr(), i)))
            .collect();
        let repr = device.repr() as *const DeviceRepr;
        Self {
            device,
            inner: Mutex::new(DeviceEventStreamInner::new()),
            repr,
            asyncs,
            async_mb,
        }
    }

    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
    }

    pub fn check_mailbox(&self, pri: MailboxPriority) -> Option<u64> {
        let mut inner = self.inner.lock().unwrap();
        inner.msg_queue[pri as usize].pop_front()
    }

    fn future_of_int(
        &self,
        inum: usize,
    ) -> impl std::future::Future<Output = Result<(usize, u64), bool>> + '_ {
        Box::pin(self.asyncs[inum].run_with(move |ii| {
            ii.repr()
                .check_for_interrupt(ii.inum)
                .ok_or(true)
                .map(|x| (inum, x))
        }))
    }

    fn future_of_mb(
        &self,
        inum: usize,
    ) -> impl std::future::Future<Output = Result<(usize, u64), bool>> + '_ {
        Box::pin(self.async_mb[inum].run_with(move |ii| {
            ii.repr()
                .check_for_mailbox(ii.inum)
                .ok_or(true)
                .map(|x| (inum, x))
        }))
    }

    fn check_add_msg(&self, i: usize) {
        if let Some(x) = self.repr().check_for_mailbox(i) {
            self.inner.lock().unwrap().msg_queue[i].push_back(x)
        }
    }

    pub async fn next(&self, int: usize) -> Option<u64> {
        if self.repr().interrupts[int].taken == 0 {
            return None;
        }
        if let Some(x) = self.repr().check_for_interrupt(int) {
            return Some(x);
        }

        let fut = self.future_of_int(int);
        fut.await.ok().map(|x| x.1)
    }

    pub async fn next_msg(&self, min: MailboxPriority) -> (MailboxPriority, u64) {
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
