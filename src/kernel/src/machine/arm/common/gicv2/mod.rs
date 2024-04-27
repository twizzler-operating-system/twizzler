/// A Generic Interrupt Controller (GIC) v2 driver interface
///
/// The full specification can be found here:
///     https://developer.arm.com/documentation/ihi0048/b?lang=en
///
/// A summary of its functionality can be found in section 10.6
/// "ARM Cortex-A Series Programmer’s Guide for ARMv8-A":
///     https://developer.arm.com/documentation/den0024/a/
mod gicc;
mod gicd;

use gicc::GICC;
use gicd::GICD;

use crate::{interrupt::Destination, memory::VirtAddr};

/// A representation of the Generic Interrupt Controller (GIC) v2
pub struct GICv2 {
    global: GICD,
    local: GICC,
}

impl GICv2 {
    // used by generic kernel interrupt code
    pub const MIN_VECTOR: usize = *GICD::SGI_ID_RANGE.start() as usize;
    pub const MAX_VECTOR: usize = *GICD::SGI_ID_RANGE.end() as usize;
    pub const NUM_VECTORS: usize =
        (*GICD::SGI_ID_RANGE.end() - *GICD::SGI_ID_RANGE.start() + 1) as usize;

    pub fn new(distr_base: VirtAddr, local_base: VirtAddr) -> Self {
        Self {
            global: GICD::new(distr_base),
            local: GICC::new(local_base),
        }
    }

    /// Configures the interrupt controller. At the end of this function
    /// the current calling CPU is ready to recieve interrupts.
    pub fn configure_local(&self) {
        // set the interrupt priority mask to accept all interrupts
        self.set_interrupt_mask(GICC::ACCEPT_ALL);

        // enable the gic cpu interface
        self.local.enable();
    }

    /// Configures global state in the interrupt controller. This should only
    /// really be called once during system intialization by the boostrap core.
    pub fn configure_global(&self) {
        // enable the gic distributor
        self.global.enable();
    }

    /// Sets the interrupt priority mask for the current calling CPU.
    fn set_interrupt_mask(&self, mask: u8) {
        // set the interrupt priority mask that we will accept
        self.local.set_interrupt_priority_mask(mask);
    }

    // Enables the interrupt with a given ID to be routed to CPUs.
    pub fn enable_interrupt(&self, int_id: u32) {
        self.global.enable_interrupt(int_id);
    }

    /// Programs the interrupt controller to be able to route
    /// a given interrupt to a particular core.
    pub fn route_interrupt(&self, int_id: u32, core: u32) {
        // route the interrupt to a corresponding core
        self.global.set_interrupt_target(int_id, core);
        // TODO: have the priority set to something reasonable
        // set the priority for the corresponding interrupt
        self.global
            .set_interrupt_priority(int_id, GICD::HIGHEST_PRIORITY);
        // TODO: edge triggered or level sensitive??? see GICD_ICFGRn
    }

    /// Returns the pending interrupt ID from the controller, and
    /// acknowledges the interrupt. Possibly returing the core ID
    /// for an SW-generated interrupt.
    pub fn pending_interrupt(&self) -> (u32, Option<u32>) {
        self.local.get_pending_interrupt_number()
    }

    /// Signal the controller that we have serviced the interrupt
    pub fn finish_active_interrupt(&self, int_id: u32, core: Option<u32>) {
        self.local.finish_active_interrupt(int_id, core);
    }

    /// Send a software generated interrupt to another core
    pub fn send_interrupt(&self, int_id: u32, dest: Destination) {
        self.global.send_interrupt(int_id, dest);
    }

    /// Print the configuration of the GIC
    pub fn print_config(&self) {
        self.global.print_config();
        self.local.print_config();
    }
}
