// This interface is temporary until we can utilize something
// like a Device Tree or ACPI during boot to describe the memory
// map in addition to the memory map the bootloader gives us.
pub mod mmio {
    use crate::memory::{MemoryRegion, MemoryRegionKind, PhysAddr};

    /// The region of the physical memory map that represents
    /// the MMIO registers of the GICv2 Distributor
    pub const GICV2_DISTRIBUTOR: MemoryRegion = MemoryRegion {
        // physical base address in QEMU
        start: unsafe {
            PhysAddr::new_unchecked(0x08000000)
        },
        // Distributor interface register map goes from 0x0000-0x1000
        length: 0x00001000,
        kind: MemoryRegionKind::Reserved,
    };

    /// The region of the physical memory map that represents
    /// the MMIO registers of the GICv2 CPU Interface
    pub const GICV2_CPU_INTERFACE: MemoryRegion = MemoryRegion {
        // physical base address in QEMU
        start: unsafe {
            PhysAddr::new_unchecked(0x08010000)
        },
        // CPU interface register map goes from 0x0000-0x1000
        // we only need 0x1004 bytes, but will request 0x2000
        length: 0x00002000,
        kind: MemoryRegionKind::Reserved,
    };

    /// The region of the physical memory map that represents
    /// the MMIO registers of the PL011 UART
    pub const PL011_UART: MemoryRegion = MemoryRegion {
        // physical base address in QEMU
        start: unsafe {
            PhysAddr::new_unchecked(0x0900_0000)
        },
        length: 0x00001000,
        kind: MemoryRegionKind::Reserved,
    };
}
