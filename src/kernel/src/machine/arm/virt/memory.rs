use crate::memory::{MemoryRegion, MemoryRegionKind, PhysAddr};

pub const DTB_ADDR: PhysAddr = unsafe {
    PhysAddr::new_unchecked(0x4000_0000)
};

static RESERVED: [MemoryRegion; 1] = [
    MemoryRegion {
        // physical base address in QEMU
        start: DTB_ADDR,
        // TODO: determine this at runtime
        length: 0x100000,
        kind: MemoryRegionKind::Reserved,
    },
];

/// A slice of physical regions of memory that are reserved
/// and should be ignored by the kernel. This list is device specific
/// and may be empty.
pub fn reserved_regions() -> &'static [MemoryRegion] {
    &RESERVED
}
