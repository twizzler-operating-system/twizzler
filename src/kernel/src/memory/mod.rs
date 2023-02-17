use alloc::boxed::Box;
use core::ops::Add;
use twizzler_abi::device::CacheType;

use crate::{arch, spinlock::Spinlock, BootInfo};

pub mod allocator;
pub mod context;
pub mod fault;
pub mod frame;
pub mod map;
pub mod pagetables;

pub use arch::{PhysAddr, VirtAddr};

use self::context::Context;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemoryRegionKind {
    UsableRam,
    Reserved,
    BootloaderReserved,
}
pub struct MemoryRegion {
    pub start: PhysAddr,
    pub length: usize,
    pub kind: MemoryRegionKind,
}
#[derive(Debug)]
pub enum MapFailed {
    FrameAllocation,
}

pub fn finish_setup() {
    //todo!()
}

pub fn init<B: BootInfo>(boot_info: &B, clone_regions: &[VirtAddr]) {
    frame::init(boot_info.memory_regions());
    let kc = context::kernel_context();
    kc.switch_to();
    allocator::init(kc);
}
