/// A Generic Interrupt Controller (GIC) v3 driver interface
///
/// The full specification can be found here:
///     https://developer.arm.com/documentation/ihi0069/latest
///
/// A summary of its functionality can be found here:
///     https://developer.arm.com/documentation/198123/0302/
use arm_gic::gicv3::{GicV3, IntId, SgiTarget};

use crate::{current_processor, interrupt::Destination, memory::VirtAddr, spinlock::Spinlock};

/// A representation of the Generic Interrupt Controller (GIC) v3
pub struct GICv3 {
    global: Spinlock<Gicv3Wrapper>,
    distr: VirtAddr,
    redist: VirtAddr,
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

    // used by generic kernel interrupt code
    pub const MIN_VECTOR: usize = 0; // *GICD::SGI_ID_RANGE.start() as usize;
    pub const MAX_VECTOR: usize = 15; //*GICD::SGI_ID_RANGE.end() as usize;
    pub const NUM_VECTORS: usize = 16;

    pub fn new(distr_base: VirtAddr, redist_base: VirtAddr) -> Self {
        unsafe {
            let gic_instance = GicV3::new(distr_base.as_mut_ptr(), redist_base.as_mut_ptr());
            Self {
                global: Spinlock::new(Gicv3Wrapper(gic_instance)),
                distr: distr_base,
                redist: redist_base,
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
    /// acknowledges the interrupt. Possibly returing the core ID
    /// for an SW-generated interrupt.
    pub fn pending_interrupt(&self) -> (u32, Option<u32>) {
        // GICv2: the IAR register contains the CPUID
        let int_id = GicV3::get_and_acknowledge_interrupt();
        // handler must read one of the Interrupt Acknowledge Registers (IARs) to get the INTID
        // multiple IAR's, IAR1 Used to acknowledge Group 1 interrupts
        (
            int_id.expect("failed to retrieve interrupt ID").into(),
            None,
        )
    }

    /// Signal the controller that we have serviced the interrupt
    pub fn finish_active_interrupt(&self, int_id: u32, _core: Option<u32>) {
        GicV3::end_interrupt(u32_to_int_id(int_id))
        // software must inform the interrupt controller: priority drop, and deactivation
        // ICC_CTLR_ELn.EOImode = 0 means that ...
        // - write to ICC_EOIR0_EL1 does deactivation and priority drop
    }

    /// Send a software generated interrupt to another core
    pub fn send_interrupt(&self, int_id: u32, dest: Destination) {
        // SGI is generated by writing to special registers in CPU interface
        // ICC_SGI1R_EL1 - current security state for PE

        // SGI's range from 0-15
        if int_id >= PPI_START {
            return;
        }

        fn sgi_target_list(core: u32) -> SgiTarget {
            use arm64::registers::MPIDR_EL1;
            use registers::{interfaces::Readable, registers::InMemoryRegister};

            let mpidr: InMemoryRegister<u64, MPIDR_EL1::Register> = {
                let core = crate::processor::get_processor(core);
                InMemoryRegister::new(core.arch.mpidr)
            };

            // target list is 16 bits
            // each bit correspongs to a PE in the cluster,
            // affinity 0 value == bit number, so to send an SGI to core 0,
            // set bit 0 high
            let target_list = 1 << mpidr.read(MPIDR_EL1::Aff0);

            SgiTarget::List {
                affinity3: mpidr.read(MPIDR_EL1::Aff3) as u8,
                affinity2: mpidr.read(MPIDR_EL1::Aff2) as u8,
                affinity1: mpidr.read(MPIDR_EL1::Aff1) as u8,
                target_list,
            }
        }

        match dest {
            Destination::Single(core) => {
                GicV3::send_sgi(u32_to_int_id(int_id), sgi_target_list(core))
            }
            Destination::AllButSelf => {
                // IRM bit controls if SGI is routed to all but self, or single
                // the GICv3 can only send interrupts to a single PE or all but self
                let all = SgiTarget::All;

                GicV3::send_sgi(u32_to_int_id(int_id), all)
            }
            Destination::Bsp | Destination::LowestPriority => {
                let bsp_core = current_processor().bsp_id();
                GicV3::send_sgi(u32_to_int_id(int_id), sgi_target_list(bsp_core))
            }
            _ => unimplemented!("unsupported SGI destination: {:?}", dest),
        };
    }

    /// Check if the interrupt is still pending.
    pub fn is_interrupt_pending(&self, _int_id: u32, dest: Destination) -> bool {
        // Checking the state of individual INTIDs
        // Distributor provides registers for state of each SPI.
        // Redistributors provide registers for state of PPIs and SGIs

        // separate registers to report the active state and the pending state

        match dest {
            _ => unimplemented!("unsupported SGI destination: {:?}", dest),
        }
    }

    /// Print the configuration of the GIC
    pub fn print_config(&self) {
        emerglogln!("[gicv3] config");
        unsafe {
            // is the controller enabled?
            let ctlr = read_reg(self.distr.as_ptr(), 0x0);
            emerglogln!("[gicv3] GICD_CTLR: {:#x}", ctlr);
            emerglogln!("\tEnableGrp: {}", get_bit(ctlr, 0));
            emerglogln!("\tEnableGrp1NS: {}", get_bit(ctlr, 1));
            emerglogln!("\tEnableGrp1S: {}", get_bit(ctlr, 2));
            emerglogln!("\tARE_S: {}", get_bit(ctlr, 4));
            emerglogln!("\tARE_NS: {}", get_bit(ctlr, 5));
            emerglogln!("\tDS: {}", get_bit(ctlr, 6));
            let mut icc: u64;
            core::arch::asm!("mrs {}, icc_igrpen0_el1", out(reg) icc);
            emerglogln!("[gicv3] ICC_IGRPEN0_EL1: {}", get_bit(icc as u32, 0));
            core::arch::asm!("mrs {}, icc_igrpen1_el1", out(reg) icc);
            emerglogln!("[gicv3] ICC_IGRPEN1_EL1: {}", get_bit(icc as u32, 0));
            // how many interrupt numbers??

            // which interrupts are enabled?
            emerglogln!("[gicv3] SGI/PPI enable");
            let renable = read_reg(self.redist.as_ptr(), 0x0100);
            // this covers SGIs and PPIs. only makes sense with affinity routing.
            emerglogln!("[gicv3] GICR_ISENABLER0: {:#x}", renable);

            // let denable = read_reg(self.distr.as_ptr(), 0x0100);
            emerglogln!(
                "[gicv3] GICD_ISENABLER0: {:#x}",
                read_reg(self.distr.as_ptr(), 0x0100)
            );
            emerglogln!(
                "[gicv3] GICD_ISENABLER1: {:#x}",
                read_reg(self.distr.as_ptr(), 0x0100 + 4 * 1)
            );
        }
    }
}

fn get_bit(reg: u32, bit: usize) -> u32 {
    (reg >> bit) & 0x1
}

/// Write a value to a single register. Registers are 32-bits wide.
unsafe fn write_reg(base: *mut u32, reg_off: usize, value: u32) {
    // let reg = (base + register as usize) as *mut u32;
    let reg = (base as *mut u8).add(reg_off) as *mut u32;
    reg.write_volatile(value as u32)
}

/// Read a value to a single register. Registers are 32-bits wide.
unsafe fn read_reg(base: *const u32, reg_off: usize) -> u32 {
    // let reg = (self.base + register as usize) as *const u32;
    let reg = (base as *const u8).add(reg_off) as *const u32;
    reg.read_volatile()
}

// The ranges for the Interrupt IDs are derived from table 2-1.

/// The ID of the first Software Generated Interrupt. SGI ranges from 0-15.
const SGI_START: u32 = 0;
/// The ID of the first Private Peripheral Interrupt. PPI ranges from 16-31.
/// Extended PPI range from 1056-1119.
const PPI_START: u32 = 16;
/// The ID of the first Shared Peripheral Interrupt. SPI ranges from 32-1019.
/// Extended SPI range from 4096-5119.
const SPI_START: u32 = 32;
/// The first special interrupt ID. Special interrupt numbers range from 1020-1023.
const SPECIAL_START: u32 = 1020;

fn u32_to_int_id(value: u32) -> IntId {
    if value < PPI_START {
        IntId::sgi(value)
    } else if value < SPI_START {
        IntId::ppi(value - PPI_START)
    } else if value < SPECIAL_START {
        IntId::spi(value - SPI_START)
    } else {
        panic!("invalid interrupt id: {}", value);
    }
}
