
use super::address::{PhysAddr, VirtAddr};

pub mod frame;
pub mod pagetables;

// start offset into physical memory. 
// 
// in the future we should determine this at runtime 
// since we don't know what the CPU supports. We might
// go about this by making `PhysAddr::get_phys_addr_width()`
// public and then calculate it that way. For now we assume
// a 48-bit physical address space.
const PHYS_MEM_OFFSET: u64 = 0xffff800000000000;

/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    VirtAddr::new(pa.raw() + PHYS_MEM_OFFSET).unwrap()
}
