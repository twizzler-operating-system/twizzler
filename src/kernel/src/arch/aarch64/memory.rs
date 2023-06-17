
use super::address::{PhysAddr, VirtAddr};

pub mod frame;
pub mod pagetables;

/// The start offset into physical memory.
///
/// The kernel is designed to run in the higher
/// half of the virtual address space, and as such expects
/// a region of virtual memory to identity map
/// all physical memory. This is convenient since
/// calculating a physical to virtual address is simply
/// va = base + offset
const PHYS_MEM_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr::new(pa.raw() + PHYS_MEM_OFFSET).unwrap()
}
