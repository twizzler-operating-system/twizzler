use crate::memory::{MemoryRegion, MemoryRegionKind, PhysAddr};

pub const DTB_ADDR: PhysAddr = unsafe { PhysAddr::new_unchecked(0x4000_0000) };

static RESERVED: [MemoryRegion; 0] = [];

/// A slice of physical regions of memory that are reserved
/// and should be ignored by the kernel. This list is device specific
/// and may be empty.
pub fn reserved_regions() -> &'static [MemoryRegion] {
    &RESERVED
}

/// Physical memory map for MMIO registers.
pub const BHYVE_UART: MemoryRegion = MemoryRegion {
    start: unsafe { PhysAddr::new_unchecked(0x0_0010_000) },
    length: 0x00001000,
    kind: MemoryRegionKind::Reserved,
};

// reg = <0x00000000 0x2f000000 0x00000000 0x00010000 0x00000000 0x2f100000 0x00000000 0x00020000>;

pub const BHYVE_GICD: MemoryRegion = MemoryRegion {
    start: unsafe { PhysAddr::new_unchecked(0x2f00_0000) },
    length: 0x00010000,
    kind: MemoryRegionKind::Reserved,
};

pub const BHYVE_GICR: MemoryRegion = MemoryRegion {
    start: unsafe { PhysAddr::new_unchecked(0x2f10_0000) },
    length: 0x00020000,
    kind: MemoryRegionKind::Reserved,
};
