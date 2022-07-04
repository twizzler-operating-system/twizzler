use std::{collections::VecDeque, sync::Mutex, task::Poll};

use futures::{future::select_all, select, FutureExt};
use twizzler_abi::{
    device::{DeviceInterrupt, DeviceRepr, MailboxPriority, NUM_DEVICE_INTERRUPTS},
    syscall::{ThreadSyncFlags, ThreadSyncReference, ThreadSyncSleep},
};
use twizzler_async::{Async, AsyncSetup};

use super::Device;

pub enum Event {
    Interrupt(u32),
    Mailbox(MailboxPriority, u64),
}

struct DeviceEventStreamInner {
    msg_queue: [VecDeque<u64>; MailboxPriority::Num as usize],
}

struct IntInner {
    inum: usize,
    repr: *const DeviceRepr,
}

impl IntInner {
    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
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
    ts: ThreadSyncSleep,
}

impl Unpin for MailboxInner {}
impl Unpin for IntInner {}

impl MailboxInner {
    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
    }
}

impl AsyncSetup for MailboxInner {
    type Error = bool;

    const WOULD_BLOCK: Self::Error = true;

    fn setup_sleep(&self) -> twizzler_abi::syscall::ThreadSyncSleep {
        self.ts
    }
}

pub struct DeviceEventStream {
    device: Device,
    inner: Mutex<DeviceEventStreamInner>,
    repr: *const DeviceRepr,
    asyncs: [Async<IntInner>; NUM_DEVICE_INTERRUPTS],
    async_mb: [Async<MailboxInner>; MailboxPriority::Num as usize],
    idx: usize,
}

impl DeviceEventStream {
    fn repr(&self) -> &DeviceRepr {
        unsafe { self.repr.as_ref().unwrap_unchecked() }
    }

    fn setup_mailbox_sleep(&self, pri: MailboxPriority) -> ThreadSyncSleep {
        let repr = self.repr();
        ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&repr.mailboxes[pri as usize]),
            0,
            twizzler_abi::syscall::ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )
    }

    pub fn check_mailbox(&self, pri: MailboxPriority) -> Option<u64> {
        let inner = self.inner.lock().unwrap();
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

    pub async fn next(&mut self) -> Event {
        loop {
            if let Some(ev) = self.check_mailbox(MailboxPriority::High) {
                return Event::Mailbox(MailboxPriority::High, ev);
            }
            if self.idx == 0 {
                if let Some(ev) = self.check_mailbox(MailboxPriority::Low) {
                    return Event::Mailbox(MailboxPriority::Low, ev);
                }
            }
            if self.repr().check_for_interrupt(self.idx).is_some() {
                return Event::Interrupt(self.idx as u32);
            }
            self.idx += 1;
            if self.idx == NUM_DEVICE_INTERRUPTS {
                self.idx = 0;
            }
            for i in 0..NUM_DEVICE_INTERRUPTS {
                if self.repr().check_for_interrupt(i).is_some() {
                    return Event::Interrupt(self.idx as u32);
                }
            }
            let futs: Vec<_> = (0..NUM_DEVICE_INTERRUPTS)
                .into_iter()
                .map(|i| self.future_of_int(i))
                .collect();

            let futs_mb: Vec<_> = (0..(MailboxPriority::Num as usize))
                .into_iter()
                .map(|i| self.future_of_mb(i))
                .collect();
            let mut top = select_all(futs).fuse();
            let mut top_m = select_all(futs_mb).fuse();
            let all = select! {
                t = top => Some(t),
                _ = top_m => {
                    for i in 0..(MailboxPriority::Num as usize) {
                        self.check_add_msg(i);
                    }
                    None
                },
            };
            if let Some(x) = all {
                return Event::Interrupt(x.0.unwrap().0 as u32);
            }
        }
    }
}
