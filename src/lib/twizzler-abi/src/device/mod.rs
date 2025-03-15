//! APIs for accessing the device tree and device representation objects.

use core::{
    fmt::Display,
    num::TryFromIntError,
    sync::atomic::{AtomicU16, AtomicU64, Ordering},
    time::Duration,
};

use crate::{
    kso::KsoHdr,
    syscall::{
        sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
        ThreadSyncSleep, ThreadSyncWake,
    },
};

pub mod bus;

pub const NUM_DEVICE_INTERRUPTS: usize = 32;

/// Possible high-level device types.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum DeviceType {
    /// An unknown device type. Should be ignored.
    Unknown = 0,
    /// A bus. This device has numerous children and should be enumerated.
    Bus = 1,
    /// A traditional "device". It may still have children, but their meaning is device-specific.
    Device = 2,
}

/// All supported kernel-discovered bus types.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum BusType {
    /// An unknown bus. Should be ignored.
    Unknown = 0,
    /// The "system" bus. Typically comprised of devices created by the kernel.
    System = 1,
    /// PCIe.
    Pcie = 2,
}

/// A device will have a number of sub-objects to present enough information and access for a
/// userspace driver to be implemented.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(u32)]
pub enum SubObjectType {
    /// An info sub-object, which is comprised of a device-specific (or bus-specific) information
    /// structure.
    Info = 0,
    /// A mapping of the MMIO registers for this device into an object.
    Mmio = 1,
}

impl From<SubObjectType> for u8 {
    fn from(x: SubObjectType) -> Self {
        match x {
            SubObjectType::Info => 0,
            SubObjectType::Mmio => 1,
        }
    }
}

impl TryFrom<u8> for SubObjectType {
    type Error = ();
    fn try_from(x: u8) -> Result<Self, ()> {
        Ok(match x {
            0 => SubObjectType::Info,
            1 => SubObjectType::Mmio,
            _ => return Err(()),
        })
    }
}

/// For MMIO registers, we may need to specify the caching type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u32)]
pub enum CacheType {
    WriteBack = 0,
    WriteCombining = 1,
    WriteThrough = 2,
    Uncacheable = 3,
    MemoryMappedIO = 4,
}

/// Info struct at the base of an mmio sub-object.
#[derive(Debug)]
#[repr(C)]
pub struct MmioInfo {
    /// The length of this mapping.
    pub length: u64,
    /// The cache type.
    pub cache_type: CacheType,
    /// Device-specific info.
    pub info: u64,
}

/// An mmio object has, at its base, a [MmioInfo] struct. At this offset, the mmio mapping actually
/// starts.
pub const MMIO_OFFSET: usize = 0x2000;

bitflags::bitflags! {
    /// Possible flags for device interrupts.
    pub struct DeviceInterruptFlags: u16 {}
}

/// A vector number (used by the kernel).
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct InterruptVector(u32);

impl TryFrom<u64> for InterruptVector {
    type Error = TryFromIntError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        let u: u32 = value.try_into()?;
        Ok(InterruptVector(u))
    }
}

impl From<InterruptVector> for u32 {
    fn from(iv: InterruptVector) -> Self {
        iv.0
    }
}

/// A per-bus device ID.
#[derive(Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Debug)]
#[repr(transparent)]
pub struct DeviceId(u32);

impl DeviceId {
    pub fn new(v: u32) -> Self {
        Self(v)
    }
}

#[repr(C)]
pub struct DeviceInterrupt {
    pub sync: AtomicU64,
    pub vec: InterruptVector,
    pub flags: DeviceInterruptFlags,
    pub taken: AtomicU16,
}

#[derive(Clone, Copy, Debug)]
pub enum MailboxPriority {
    Idle,
    Low,
    High,
    Num,
}

impl TryFrom<usize> for MailboxPriority {
    type Error = ();

    fn try_from(value: usize) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => MailboxPriority::Idle,
            1 => MailboxPriority::Low,
            2 => MailboxPriority::High,
            3 => MailboxPriority::Num,
            _ => return Err(()),
        })
    }
}
/// The base struct for a device object.
#[repr(C)]
pub struct DeviceRepr {
    kso_hdr: KsoHdr,
    pub device_type: DeviceType,
    pub bus_type: BusType,
    pub device_id: DeviceId,
    pub interrupts: [DeviceInterrupt; NUM_DEVICE_INTERRUPTS],
    pub mailboxes: [AtomicU64; MailboxPriority::Num as usize],
}

impl Display for DeviceRepr {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "Device::{{{:?}, {:?}, {:?}}}({})",
            self.device_type, self.bus_type, self.device_id, self.kso_hdr
        )
    }
}

impl DeviceRepr {
    /// Construct a new device repr.
    pub fn new(
        kso_hdr: KsoHdr,
        device_type: DeviceType,
        bus_type: BusType,
        device_id: DeviceId,
    ) -> Self {
        // Clippy complains about this, but I don't know another way to do it cleanly.
        // There probably is one, but I don't know it. Anyways, this is fine because we're
        // never writing to V.
        #[allow(clippy::declare_interior_mutable_const)]
        const V: DeviceInterrupt = DeviceInterrupt {
            sync: AtomicU64::new(0),
            vec: InterruptVector(0),
            flags: DeviceInterruptFlags::empty(),
            taken: AtomicU16::new(0),
        };

        #[allow(clippy::declare_interior_mutable_const)]
        const M: AtomicU64 = AtomicU64::new(0);
        Self {
            kso_hdr,
            device_type,
            bus_type,
            device_id,
            interrupts: [V; NUM_DEVICE_INTERRUPTS],
            mailboxes: [M; MailboxPriority::Num as usize],
        }
    }

    /// Block until an interrupt fires.
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

    pub fn setup_interrupt_sleep(&self, inum: usize) -> ThreadSyncSleep {
        ThreadSyncSleep {
            reference: ThreadSyncReference::Virtual(&self.interrupts[inum].sync),
            value: 0,
            op: ThreadSyncOp::Equal,
            flags: ThreadSyncFlags::empty(),
        }
    }

    pub fn submit_mailbox_msg(&self, mb: MailboxPriority, msg: u64) {
        while self.mailboxes[mb as usize]
            .compare_exchange(0, msg, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            core::hint::spin_loop()
        }
        let _ = sys_thread_sync(
            &mut [ThreadSync::new_wake(ThreadSyncWake::new(
                ThreadSyncReference::Virtual(&self.mailboxes[mb as usize]),
                usize::MAX,
            ))],
            None,
        );
    }

    /// Poll an interrupt vector to see if it has fired.
    pub fn check_for_interrupt(&self, inum: usize) -> Option<u64> {
        let val = self.interrupts[inum].sync.swap(0, Ordering::SeqCst);
        if val == 0 {
            None
        } else {
            Some(val)
        }
    }

    /// Poll an interrupt vector to see if it has fired.
    pub fn check_for_mailbox(&self, inum: usize) -> Option<u64> {
        let val = self.mailboxes[inum].swap(0, Ordering::SeqCst);
        if val == 0 {
            None
        } else {
            Some(val)
        }
    }

    /// Register an interrupt vector with this device.
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
