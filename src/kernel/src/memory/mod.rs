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

fn init_kernel_context(clone_regions: &[VirtAddr]) -> MemoryContextInner {
    loop {}
    let ctx = MemoryContextInner::current();
    let mut new_context = MemoryContextInner::new_blank();

    let phys_mem_offset = arch::memory::phys_to_virt(PhysAddr::new(0).unwrap());
    /* TODO: map ALL of the physical address space */

    todo!()
}

pub fn finish_setup() {
    todo!()
}

pub fn init<B: BootInfo>(boot_info: &B, clone_regions: &[VirtAddr]) {
    frame::init(boot_info.memory_regions());
    let kernel_context = init_kernel_context(clone_regions);

    todo!()

    //allocator::init(kernel_memory_manager());
}
