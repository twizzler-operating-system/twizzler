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
/// va = base + offset. Its value is set at early kernel
/// initialization and currently is bootloader-specific.
pub(super) static mut PHYS_MEM_OFFSET: u64 = 0;

// TODO: choose where our own identity map lives

/* TODO: hide this */
pub fn phys_to_virt(pa: PhysAddr) -> VirtAddr {
    let raw: u64 = pa.into();
    VirtAddr::new(raw + unsafe { PHYS_MEM_OFFSET }).unwrap()
}
