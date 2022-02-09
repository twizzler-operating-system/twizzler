use core::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::{
    kso::KsoHdr,
    syscall::{ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep},
};

pub mod bus;

const NUM_DEVICE_INTERRUPTS: usize = 32;

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum DeviceType {
    Unknown = 0,
    Bus = 1,
    Device = 2,
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum BusType {
    Unknown = 0,
    System = 1,
    Pcie = 2,
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum SubObjectType {
    Info = 0,
    Mmio = 1,
}

bitflags::bitflags! {
    pub struct DeviceInterruptFlags: u16 {}
}

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct InterruptVector(u32);

#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct DeviceId(u32);

impl DeviceId {
    pub fn new(v: u32) -> Self {
        Self(v)
    }
}

#[repr(C)]
struct DeviceInterrupt {
    sync: AtomicU64,
    vec: InterruptVector,
    flags: DeviceInterruptFlags,
    taken: u16,
}

#[repr(C)]
pub struct DeviceRepr {
    kso_hdr: KsoHdr,
    device_type: DeviceType,
    bus_type: BusType,
    device_id: DeviceId,
    interrupts: [DeviceInterrupt; NUM_DEVICE_INTERRUPTS],
}

impl DeviceRepr {
    pub fn new(
        kso_hdr: KsoHdr,
        device_type: DeviceType,
        bus_type: BusType,
        device_id: DeviceId,
    ) -> Self {
        const V: DeviceInterrupt = DeviceInterrupt {
            sync: AtomicU64::new(0),
            vec: InterruptVector(0),
            flags: DeviceInterruptFlags::empty(),
            taken: 0,
        };
        Self {
            kso_hdr,
            device_type,
            bus_type,
            device_id,
            interrupts: [V; NUM_DEVICE_INTERRUPTS],
        }
    }

    pub fn wait_for_interrupt(&self, inum: usize, timeout: Option<Duration>) -> u64 {
        loop {
            let val = self.interrupts[inum].sync.swap(0, Ordering::SeqCst);
            if val != 0 {
                return val;
            }
            // Spin for a bit
            for _ in 0..100 {
                let val = self.interrupts[inum].sync.load(Ordering::SeqCst);
                if val != 0 {
                    return self.interrupts[inum].sync.swap(0, Ordering::SeqCst);
                }
            }
            let op = ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&self.interrupts[inum].sync as *const AtomicU64),
                0,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ));
            let res = crate::syscall::sys_thread_sync(&mut [op], timeout);
            if res.is_err() {
                return 0;
            }
        }
    }

    pub fn check_for_interrupt(&self, inum: usize) -> Option<u64> {
        let val = self.interrupts[inum].sync.swap(0, Ordering::SeqCst);
        if val == 0 {
            None
        } else {
            Some(val)
        }
    }

    pub fn register_interrupt(
        &mut self,
        inum: usize,
        vec: InterruptVector,
        flags: DeviceInterruptFlags,
    ) {
        self.interrupts[inum].vec = vec;
        self.interrupts[inum].flags = flags;
        self.interrupts[inum].sync = AtomicU64::new(0);
    }
}
