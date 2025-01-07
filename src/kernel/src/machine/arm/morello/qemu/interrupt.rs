use lazy_static::lazy_static;

use crate::machine::arm::common::gicv3::GICv3;

// used by generic kernel interrupt code
pub const MIN_VECTOR: usize = GICv3::MIN_VECTOR;
pub const MAX_VECTOR: usize = GICv3::MAX_VECTOR;
pub const NUM_VECTORS: usize = GICv3::NUM_VECTORS;

lazy_static! {
    /// System-wide reference to the interrupt controller
    pub static ref INTERRUPT_CONTROLLER: GICv3 = {
        use twizzler_abi::{device::CacheType, object::Protections};

        use crate::memory::{
            PhysAddr,
            pagetables::{
                ContiguousProvider, MappingCursor, MappingSettings, Mapper,
                MappingFlags,
            },
        };
        use crate::arch::memory::mmio::MMIO_ALLOCATOR;

        // retrive the locations of the MMIO registers
        let (distributor_mmio, cpu_interface_mmio) = crate::machine::info::get_gicv3_info();
        // reserve regions of virtual address space for MMIO
        let (gicr_mmio_base, gicd_mmio_base) = {
            let mut alloc = MMIO_ALLOCATOR.lock();
            let cpu = alloc.alloc(cpu_interface_mmio.length as usize)
                .expect("failed to allocate MMIO region");
            let dist = alloc.alloc(distributor_mmio.length as usize)
                .expect("failed to allocate MMIO region");
            (cpu, dist)
        };
        // configure mapping settings for this region of memory
        let gicr_region = MappingCursor::new(
            gicr_mmio_base,
            cpu_interface_mmio.length as usize,
        );
        let mut gicr_phys = ContiguousProvider::new(
            unsafe { PhysAddr::new_unchecked(cpu_interface_mmio.info) },
            cpu_interface_mmio.length as usize,
        );
        let gicd_region = MappingCursor::new(
            gicd_mmio_base,
            distributor_mmio.length as usize,
        );
        let mut gicd_phys = ContiguousProvider::new(
            unsafe { PhysAddr::new_unchecked(distributor_mmio.info) },
            distributor_mmio.length as usize,
        );
        // Device memory only prevetns speculative data accesses, so we must not
        // make this region executable to prevent speculative instruction accesses.
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::MemoryMappedIO,
            MappingFlags::GLOBAL,
        );
        // map in with curent memory context
        unsafe {
            let mut mapper = Mapper::current();
            mapper.map(gicr_region, &mut gicr_phys, &settings);
            mapper.map(gicd_region, &mut gicd_phys, &settings);
        }
        GICv3::new(
            gicd_mmio_base,
            gicr_mmio_base,
        )
    };
}
