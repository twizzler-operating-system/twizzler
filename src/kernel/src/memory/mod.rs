use crate::{arch, BootInfo};

pub mod allocator;
pub mod context;
pub mod frame;
pub mod pagetables;

pub use arch::{PhysAddr, VirtAddr};

use self::context::{KernelMemoryContext, UserContext};

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

pub fn finish_setup() {
    //todo!()
}

pub fn init<B: BootInfo>(boot_info: &B) {
    frame::init(boot_info.memory_regions());
    let kc = context::kernel_context();
    kc.switch_to();
    kc.init_allocator();
    allocator::init(kc);
}
