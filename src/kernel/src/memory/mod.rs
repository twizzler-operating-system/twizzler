use core::sync::atomic::{AtomicBool, Ordering};

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

pub fn init<B: BootInfo>(boot_info: &B) {
    frame::init(boot_info.memory_regions());
    let kc = context::kernel_context();
    kc.switch_to();
    kc.init_allocator();
    allocator::init(kc);
    // set flag to indicate that mm system is initalized
    MEM_INIT.store(true, Ordering::SeqCst);
}

static MEM_INIT: AtomicBool = AtomicBool::new(false);

/// Indicates if memory management has been initalized by the boot core.
pub fn is_init() -> bool {
    MEM_INIT.load(Ordering::SeqCst)
}

pub fn prep_smp() {
    let kc = context::kernel_context();
    kc.prep_smp();
}
