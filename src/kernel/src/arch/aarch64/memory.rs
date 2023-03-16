
use super::address::{PhysAddr, VirtAddr};

pub mod frame;
pub mod pagetables;

// TODO:
// start offset into physical memory
const PHYS_MEM_OFFSET: u64 = 0x0;

/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    let raw: u64 = pa.into();
    VirtAddr::new(raw + PHYS_MEM_OFFSET).unwrap()
}
