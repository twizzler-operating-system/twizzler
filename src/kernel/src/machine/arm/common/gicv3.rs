/// A Generic Interrupt Controller (GIC) v3 driver interface
///
/// The full specification can be found here:
///     https://developer.arm.com/documentation/ihi0069/latest
///
/// A summary of its functionality can be found here:
///     https://developer.arm.com/documentation/198123/0302/
///
/// A driver interface for the GICv3

use arm_gic::gicv3::{GicV3, IntId};

use crate::{memory::VirtAddr, spinlock::Spinlock};

/// A representation of the Generic Interrupt Controller (GIC) v3
pub struct GICv3 {
    global: Spinlock<Gicv3Wrapper>,
}

/// This wrapper is needed because the type `GicV3` is not
/// Send or Sync since it's internal implementation uses
/// raw pointers. The workaround is a wrapper type so we
/// are able to use this with `Spinlock` and be able to
/// use its APIs that mutate state in the global distributor.
struct Gicv3Wrapper(GicV3);

unsafe impl Send for Gicv3Wrapper {}
unsafe impl Sync for Gicv3Wrapper {}

impl GICv3 {
    // The interrupt mask to accept all interrupts regardless of priority.
    const ACCEPT_ALL: u8 = 0xff;

    // The highest interrupt priority. Lower means higher priority.
    const HIGHEST_PRIORITY: u8 = 0;

    pub fn new(distr_base: VirtAddr, redist_base: VirtAddr) -> Self {
        unsafe {
            let gic_instance = GicV3::new(distr_base.as_mut_ptr(), redist_base.as_mut_ptr());
            Self {
                global: Spinlock::new(Gicv3Wrapper(gic_instance)),
            }
        }
    }

    /// Configures the interrupt controller. At the end of this function
    /// the current calling CPU is ready to recieve interrupts.
    pub fn configure_local(&self) {
        // set the interrupt priority mask to accept all interrupts
        self.set_interrupt_mask(Self::ACCEPT_ALL);
        // TODO: enable the gic cpu interface. See GICR_WAKER

        // NOTE: handled in the `setup` function. This might not be the best
        // crate to use if we want to utilize multiple cores since
        // the gicv3 crate touches global and local state during `setup`.
    }

    /// Configures global state in the interrupt controller. This should only
    /// really be called once during system intialization by the boostrap core.
    pub fn configure_global(&self) {
        // enable the gic distributor
        self.global.lock().0.setup();
        // NOTE: This might not be the best crate to use if we want to
        // utilize multiple cores since the gicv3 crate touches global
        // and local state during `setup`.
        //
        // might be OK since the global configs are the same every time.
    }

    /// Sets the interrupt priority mask for the current calling CPU.
    fn set_interrupt_mask(&self, mask: u8) {
        // set the interrupt priority mask that we will accept
        GicV3::set_priority_mask(mask);
    }

    // Enables the interrupt with a given ID to be routed to CPUs.
    pub fn enable_interrupt(&self, int_id: u32) {
        self.global
            .lock()
            .0
            .enable_interrupt(u32_to_int_id(int_id), true);
    }

    /// Programs the interrupt controller to be able to route
    /// a given interrupt to a particular core.
    pub fn route_interrupt(&self, int_id: u32, _core: u32) {
        // TODO: route interrupts (PPIs/SPIs) to cores
        // route the interrupt to a corresponding core
        // self.global.set_interrupt_target(int_id, core);

        // TODO: have the priority set to something reasonable
        // set the priority for the corresponding interrupt
        self.global
            .lock()
            .0
            .set_interrupt_priority(u32_to_int_id(int_id), Self::HIGHEST_PRIORITY);
        // TODO: edge triggered or level sensitive??? see GICD_ICFGRn
    }

    /// Returns the pending interrupt ID from the controller, and
    /// acknowledges the interrupt.
    pub fn pending_interrupt(&self) -> u32 {
        let int_id = GicV3::get_and_acknowledge_interrupt();
        int_id.expect("failed to retrieve interrupt ID").into()
    }

    /// Signal the controller that we have serviced the interrupt
    pub fn finish_active_interrupt(&self, int_id: u32) {
        GicV3::end_interrupt(u32_to_int_id(int_id))
    }

    /// Print the configuration of the GIC
    pub fn print_config(&self) {
        todo!()
    }
}

/// The ID of the first Software Generated Interrupt.
const SGI_START: u32 = 0;
/// The ID of the first Private Peripheral Interrupt.
const PPI_START: u32 = 16;
/// The ID of the first Shared Peripheral Interrupt.
const SPI_START: u32 = 32;
/// The first special interrupt ID.
const SPECIAL_START: u32 = 1020;

fn u32_to_int_id(value: u32) -> IntId {
    logln!("[debug] setting int id: {}", value);
    if value < PPI_START {
        IntId::sgi(PPI_START - value)
    } else if value < SPI_START {
        IntId::ppi(SPI_START - value)
    } else if value < SPECIAL_START {
        IntId::spi(SPECIAL_START - value)
    } else {
        panic!("invalid interrupt id: {}", value);
    }
}
