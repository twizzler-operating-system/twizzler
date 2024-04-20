use super::address::{PhysAddr, VirtAddr};

pub mod frame;
pub mod pagetables;

const PHYS_MEM_OFFSET: u64 = 0xffff800000000000;
/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    let raw: u64 = pa.into();
    VirtAddr::new(raw + PHYS_MEM_OFFSET).unwrap()
}
