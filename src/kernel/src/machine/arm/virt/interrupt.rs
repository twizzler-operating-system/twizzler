use lazy_static::lazy_static;

use super::super::gicv2::GICv2;

use crate::machine::memory::mmio::{GICV2_DISTRIBUTOR, GICV2_CPU_INTERFACE};

lazy_static! {
    /// System-wide reference to the interrupt controller
    pub static ref INTERRUPT_CONTROLLER: GICv2 = {
        GICv2::new(
            // TODO: might need to lock global distributor state,
            // an possibly CPU interface
            GICV2_DISTRIBUTOR.start.kernel_vaddr(),
            GICV2_CPU_INTERFACE.start.kernel_vaddr(),
        )
    };
}
