use lazy_static::lazy_static;

use super::super::gicv2::GICv2;

use crate::machine::memory::mmio::{GICV2_DISTRIBUTOR, GICV2_CPU_INTERFACE};

lazy_static! {
    /// System-wide reference to the interrupt controller
    pub static ref INTERRUPT_CONTROLLER: GICv2 = {
        use twizzler_abi::{device::CacheType, object::Protections};
        
        use crate::memory::pagetables::{
            ContiguousProvider, MappingCursor, MappingSettings, Mapper,
            MappingFlags,
        };
        let gicc_mmio_base = GICV2_CPU_INTERFACE.start.kernel_vaddr();
        let gicd_mmio_base = GICV2_DISTRIBUTOR.start.kernel_vaddr();
        // configure mapping settings for this region of memory
        let gicc_region = MappingCursor::new(
            gicc_mmio_base,
            GICV2_CPU_INTERFACE.length,
        );
        let mut gicc_phys = ContiguousProvider::new(
            GICV2_CPU_INTERFACE.start,
            GICV2_CPU_INTERFACE.length,
        );
        let gicd_region = MappingCursor::new(
            gicd_mmio_base,
            GICV2_DISTRIBUTOR.length,
        );
        let mut gicd_phys = ContiguousProvider::new(
            GICV2_DISTRIBUTOR.start,
            GICV2_DISTRIBUTOR.length,
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
            // an possibly CPU interface
            gicd_mmio_base,
            gicc_mmio_base,
        )
    };
}
