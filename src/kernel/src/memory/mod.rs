use core::sync::atomic::{AtomicBool, Ordering};

use crate::{arch, security::KERNEL_SCTX, BootInfo};

pub mod allocator;
pub mod context;
pub mod frame;
pub mod pagetables;

pub use arch::{PhysAddr, VirtAddr};
use twizzler_abi::object::NULLPAGE_SIZE;

use self::context::{KernelMemoryContext, UserContext};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MemoryRegionKind {
    UsableRam,
    Reserved,
    BootloaderReserved,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryRegion {
    pub start: PhysAddr,
    pub length: usize,
    pub kind: MemoryRegionKind,
}

impl MemoryRegion {
    pub fn split(mut self, len: usize) -> Option<(MemoryRegion, MemoryRegion)> {
        let len = len.next_multiple_of(NULLPAGE_SIZE);
        if self.length <= len {
            return None;
        }
        let mut second = self;
        second.start = self.start.offset(len).ok()?;
        second.length -= len;
        self.length = len;
        Some((self, second))
    }
}

pub fn init(boot_info: &dyn BootInfo) {
    frame::init(boot_info.memory_regions());
    let kc = context::kernel_context();
    kc.switch_to(KERNEL_SCTX);
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
