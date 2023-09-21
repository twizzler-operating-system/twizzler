use lazy_static::lazy_static;

use super::super::common::gicv2::GICv2;

lazy_static! {
    /// System-wide reference to the interrupt controller
    pub static ref INTERRUPT_CONTROLLER: GICv2 = {
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
        let (distributor_mmio, cpu_interface_mmio) = crate::machine::info::get_gicv2_info();
        // reserve regions of virtual address space for MMIO
        let (gicc_mmio_base, gicd_mmio_base) = {
            let mut alloc = MMIO_ALLOCATOR.lock();
            let cpu = alloc.alloc(cpu_interface_mmio.length as usize)
                .expect("failed to allocate MMIO region");
            let dist = alloc.alloc(distributor_mmio.length as usize)
                .expect("failed to allocate MMIO region");
            (cpu, dist)
        };
        // configure mapping settings for this region of memory
        let gicc_region = MappingCursor::new(
            gicc_mmio_base,
            cpu_interface_mmio.length as usize,
        );
        let mut gicc_phys = ContiguousProvider::new(
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
            mapper.map(gicc_region, &mut gicc_phys, &settings);
            mapper.map(gicd_region, &mut gicd_phys, &settings);
        }
        GICv2::new(
            // TODO: might need to lock global distributor state,
            // and possibly CPU interface
            gicd_mmio_base,
            gicc_mmio_base,
        )
    };
}
