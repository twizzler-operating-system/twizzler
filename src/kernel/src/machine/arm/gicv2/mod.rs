mod gicd;
mod gicc;
mod mmio;

use gicd::GICD;
use gicc::GICC;

use crate::memory::VirtAddr;

pub struct GICv2 {
    global: GICD,
    local: GICC,
}

impl GICv2 {
    pub fn new(distr_base: VirtAddr, local_base: VirtAddr) -> Self {
        Self {
            global: GICD::new(distr_base),
            local: GICC::new(local_base),
        }
    }

    /// Configures the interrupt controller. At the end of this function
    /// the current calling CPU is ready to recieve interrupts.
    pub fn configure(&self) {
        // enable the gic distributor
        self.global.enable();

        // set the interrupt priority mask to accept all interrupts
        self.set_interrupt_mask(GICC::ACCEPT_ALL);

        // enable the gic cpu interface
        self.local.enable();
    }

    /// Sets the interrupt priority mask for the current calling CPU.
    fn set_interrupt_mask(&self, mask: u8) {
        // set the interrupt priority mask that we will accept
        self.local.set_interrupt_priority_mask(mask);
    }

    // Enables the interrupt with a given ID to be routed to CPUs.
    pub fn enable_interrupt(&self, int_id: u32) {
        self.global.enable_interrupt(int_id);

        // TODO: set the priority for the corresponding interrupt? see GICD_IPRIORITYRn
        // TODO: edge triggered or level sensitive??? see GICD_ICFGRn
    }

    /// Programs the interrupt controller to be able to route
    /// a given interrupt to a particular core.
    fn route_interrupt(&self, _int_id: u32, _core: u32) {
        // TODD: route the interrupt to a corresponding core, see GICD_ITARGETSRn
        todo!()
    }

    /// Returns the pending interrupt ID from the controller, and
    /// acknowledges the interrupt.
    pub fn pending_interrupt(&self) -> u32 {
        self.local.get_pending_interrupt_number()
    }

    /// Signal the controller that we have serviced the interrupt
    pub fn finish_active_interrupt(&self, int_id: u32) {
        self.local.finish_active_interrupt(int_id);
    }

    /// Print the configuration of the GIC
    pub fn print_config(&self) {
        self.global.print_config();
        self.local.print_config();
    }
}

