use crate::memory::VirtAddr;

pub struct GICv2;



impl GICv2 {
    pub fn new(_distr_base: VirtAddr, _local_base: VirtAddr) -> Self {
        Self { }
    }

    /// Configures the interrupt controller. At the end of this function
    /// the current calling CPU is ready to recieve interrupts.
    pub fn configure(&self) {
        todo!()
    }

    /// Sets the interrupt priority mask for the current calling CPU.
    fn set_interrupt_mask(&self, mask: u8) {
        todo!()
    }

    // Enables the interrupt with a given ID to be routed to CPUs.
    fn enable_interrupt(&self, int_id: u32) {
        todo!()
    }

    /// Programs the interrupt controller to be able to route
    /// a given interrupt to a particular core.
    fn route_interrupt(&self, int_id: u32, core: u32) {
        todo!()
    }

    /// Returns the pending interrupt ID from the controller, and
    /// acknowledges the interrupt.
    fn pending_interrupt(&self) -> u32 {
        todo!()
    }

    /// Signal the controller that we have serviced the interrupt
    fn finish_active_interrupt(&self, int_id: u32) {
        todo!()
    }

}