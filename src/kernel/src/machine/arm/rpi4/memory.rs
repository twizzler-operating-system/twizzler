// This interface is temporary until we can utilize something
// like a Device Tree or ACPI during boot to describe the memory
// map in addition to the memory map the bootloader gives us.
//
// More information about the RPi4 and its perihperals can be found
// in the "BCM2711 ARM Peripherals" document here:
//     https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf


use crate::memory::{MemoryRegion, MemoryRegionKind, PhysAddr};

pub const DTB_ADDR: PhysAddr = unsafe {
    PhysAddr::new_unchecked(0x4000_0000)
};

static RESERVED: [MemoryRegion; 0] = [];

/// A slice of physical regions of memory that are reserved
/// and should be ignored by the kernel. This list is device specific
/// and may be empty.
pub fn reserved_regions() -> &'static [MemoryRegion] {
    &RESERVED
}

pub mod mmio {
    use crate::memory::{MemoryRegion, MemoryRegionKind, PhysAddr};

    /// The region of the physical memory map that represents
    /// the MMIO registers of the GICv2 Distributor
    pub const GICV2_DISTRIBUTOR: MemoryRegion = MemoryRegion {
        // physical base address in QEMU
        start: unsafe {
            PhysAddr::new_unchecked(0x0_FF84_1000)
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
            PhysAddr::new_unchecked(0x0_FF84_2000)
        },
        // CPU interface register map goes from 0x0000-0x1000
        // we only need 0x1004 bytes, but will request 0x2000
        length: 0x00002000,
        kind: MemoryRegionKind::Reserved,
    };

    /// The region of the physical memory map that represents
    /// the MMIO registers of the PL011 UART
    pub const PL011_UART: MemoryRegion = MemoryRegion {
        // physical base address in the Raspberry Pi 4
        // according to Ch. 11.5/1.2.4
        start: unsafe {
            PhysAddr::new_unchecked(0x0_FE20_1000)
        },
        length: 0x00001000,
        kind: MemoryRegionKind::Reserved,
    };

    /// The region of the physical memory map that represents
    /// the MMIO registers of the GPIO pin registers
    pub const GPIO_PINS: MemoryRegion = MemoryRegion {
        // physical base address in the Raspberry Pi 4
        // according to Ch. 5.2/1.2.4
        start: unsafe {
            PhysAddr::new_unchecked(0x0_FE20_0000)
        },
        // length of the GPIO MMIO is 0xf4 bytes,
        // we'll just take a page ...
        length: 0x00001000,
        kind: MemoryRegionKind::Reserved,
    };
}
