/// A Generic Interrupt Controller (GIC) v2 Distributor Interface
///
/// The full specification can be found here:
///     https://developer.arm.com/documentation/ihi0048/b?lang=en
///
/// Relevant sections include, but are not limited to: 2.2, 4.1.2, 4.3
///
/// A summary of its functionality can be found in section 10.6
/// "ARM Cortex-A Series Programmer’s Guide for ARMv8-A":
///     https://developer.arm.com/documentation/den0024/a/
use core::ops::RangeInclusive;

use registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite, WriteOnly},
};

use super::super::mmio::MmioRef;
use crate::{current_processor, interrupt::Destination, memory::VirtAddr};

// Each register in the specification is prefixed with GICD_
register_bitfields! {
    u32,

    /// Distributor Control Register
    CTLR [
        // bits [31:1] are reserved
        Enable OFFSET(0) NUMBITS(1) []
    ],

    /// Interrupt Controller Type Register
    TYPER [
        // bits [31:16] are reserved
        CPUNumber OFFSET(5)  NUMBITS(3) [],
        ITLinesNumber OFFSET(0)  NUMBITS(5) []
    ],

    /// Distributor Implementer Identification Register
    IIDR [
        ProductID OFFSET(24) NUMBITS(8) [],
        // bits [23:20] are reserved
        Variant OFFSET(16) NUMBITS(4) [],
        Revision OFFSET(12) NUMBITS(4) [],
        Implementer OFFSET(0)  NUMBITS(11) []
    ],

    /// Interrupt Processor Targets Registers
    ITARGETSR [
        TargetOffset3 OFFSET(24) NUMBITS(8) [],
        TargetOffset2 OFFSET(16) NUMBITS(8) [],
        TargetOffset1 OFFSET(8)  NUMBITS(8) [],
        TargetOffset0 OFFSET(0)  NUMBITS(8) []
    ],

    /// Interrupt Priority Registers
    IPRIORITYR [
        PriorityOffset3 OFFSET(24) NUMBITS(8) [],
        PriorityOffset2 OFFSET(16) NUMBITS(8) [],
        PriorityOffset1 OFFSET(8)  NUMBITS(8) [],
        PriorityOffset0 OFFSET(0)  NUMBITS(8) []
    ],

    /// Software Generated Interrupt Register
    SGIR [
        // bits [31:26] are reserved
        TargetListFilter OFFSET(24)  NUMBITS(2) [
            ForwardSpecified = 0b00,
            AllButSelf = 0b01,
            ToSelf = 0b10,
            // the value 0b11 is reserved
        ],
        CPUTargetList OFFSET(16)  NUMBITS(8) [],
        IntID OFFSET(0) NUMBITS(4) [],
        // NSATT
    ],

    /// SGI Clear-Pending Registers
    CPENDSGIR [
        SGI_M3 OFFSET(24) NUMBITS(8) [],
        SGI_M2 OFFSET(16) NUMBITS(8) [],
        SGI_M1 OFFSET(8) NUMBITS(8) [],
        SGI_M0 OFFSET(0) NUMBITS(8) [],
    ],
}

// Each register in the specification is prefixed with GICD_
register_structs! {
    /// Distributor Register Map according
    /// to Section 4.1.2, Table 4-1.
    /// All registers are 32-bits wide.
    #[allow(non_snake_case)]
    DistributorRegisters {
        /// Distributor Control Register
        (0x000 => CTLR: ReadWrite<u32, CTLR::Register>),
        /// Interrupt Controller Type Register
        (0x004 => TYPER: ReadOnly<u32, TYPER::Register>),
        (0x008 => IIDR: ReadOnly<u32, IIDR::Register>),
        (0x00C => _reserved1),
        /// Interrupt Set-Enable Registers
        (0x100 => ISENABLER: [ReadWrite<u32>; 32]),
        (0x180 => _reserved2),
        (0x400 => IPRIORITYR: [ReadWrite<u32, IPRIORITYR::Register>; 255]),
        (0x7FC => _reserved3),
        // skip the banked ITARGETSR registers, see 4.3.12
        (0x800 => ITARGETSR_BANKED: [ReadOnly<u32, ITARGETSR::Register>; 8]),
        // this covers interrupt numbers 32 - 1019
        (0x820 => ITARGETSR: [ReadWrite<u32, ITARGETSR::Register>; 247]),
        (0xBFC => _reserved4),
        /// Software Generated Interrupt Register
        (0xF00 => SGIR: WriteOnly<u32, SGIR::Register>),
        (0xF04 => _reserved5),
        /// SGI Clear-Pending Registers
        (0xF10 => CPENDSGIR: [ReadWrite<u32, CPENDSGIR::Register>; 4]),
        (0xF20 => @END),
    }
}

/// GIC Distributor
pub struct GICD {
    registers: MmioRef<DistributorRegisters>,
}

impl GICD {
    /// According to 4.3.11 and 3.3: "GICD_IPRIORITYRs provide 8-bit
    /// priority field for each interrupt," and Lower numbers have
    /// higher priority, with 0 being the highest.
    pub const HIGHEST_PRIORITY: u8 = 0;

    // The CPU can see interrupt IDs 0-1019. 0-31 are banked by
    // the distributor and uniquely seen by each processor, and
    // SPIs range from 32-1019 (2.2.1).

    /// Software Generated Interrupts (SGIs) range from 0-15 (See 2.2.1)
    pub(super) const SGI_ID_RANGE: RangeInclusive<u32> = RangeInclusive::new(0, 15);

    /// Private Peripheral Interrupts (PPIs) range from 16-31 (See 2.2.1)
    const PPI_ID_RANGE: RangeInclusive<u32> = RangeInclusive::new(16, 31);

    /// Shared Peripheral Interrupts (SPIs) range from 32-1019 (See 2.2.1)
    const SPI_ID_RANGE: RangeInclusive<u32> = RangeInclusive::new(32, 1019);

    pub fn new(base: VirtAddr) -> Self {
        Self {
            registers: MmioRef::new(base.as_ptr::<DistributorRegisters>()),
        }
    }

    /// enable the distributor interface
    pub fn enable(&self) {
        // enable forwarding of pending interrupts from Distributor to CPU interfaces.
        self.registers.CTLR.write(CTLR::Enable::SET);
    }

    /// print the configuration of the distributor
    pub fn print_config(&self) {
        emerglogln!("[gic::gicd] printing configuration");
        // is it enabled?
        let enabled = self.registers.CTLR.read(CTLR::Enable);
        emerglogln!("\tCTLR::Enable: {}", enabled);
        // how many interrupts does it support?
        let itl = self.registers.TYPER.read(TYPER::ITLinesNumber);
        emerglogln!("\tTYPER::ITLinesNumber: N={} => {}", itl, 32 * (itl + 1));
        // how many cpus does it support?
        let cpus = self.registers.TYPER.read(TYPER::CPUNumber);
        emerglogln!("\tTYPER::CPUNumber: {}", cpus + 1);
        // how many set enable registers are there?
        let num_int_enable = itl + 1;
        emerglogln!("\tNumber of ISENABLER registers: {}", num_int_enable);
        // dump the enable state for ISENABLER0
        let local_ints = self.registers.ISENABLER[0].get();
        emerglogln!("\tISENABLER0: {:#x}", local_ints);
        // what is the interurpt mask for local interrupts routed?
        // a read of an of the cpu targets returns the number(core id) of the processor reading it
        let int_mask = self.registers.ITARGETSR_BANKED[0].read(ITARGETSR::TargetOffset0);
        emerglogln!("\tITARGETSR[0]: cpu number {:#x}", int_mask);
    }

    /// Set the enable bit for the corresponding interrupt.
    pub fn enable_interrupt(&self, int_id: u32) {
        // The GICD_ISENABLERns provide the set-enable bits
        // for each interrupt shared or otherwise (3.1.2).
        //
        // NOTE: The implementation of SGIs may have them
        // permanently enabled or they need to be manually
        // enabled/disabled
        if Self::SGI_ID_RANGE.contains(&int_id)
            || Self::PPI_ID_RANGE.contains(&int_id)
            || Self::SPI_ID_RANGE.contains(&int_id)
        {
            // according to the algorithm on 4-93:
            // 1. GICD_ISENABLER n = int_id / 32
            let iser = (int_id / 32) as usize;
            // 2. bit number = int_id % 32
            let bit_index = int_id % 32;

            // First, we read the right GICD_ISENABLER register
            let mut enable = self.registers.ISENABLER[iser].get();
            // set the bit in the local copy
            enable = enable | (1 << bit_index);
            // then write out the value
            self.registers.ISENABLER[iser].set(enable);
        } else {
            unimplemented!("unsupported interrupt number: {}", int_id)
        }
    }

    /// configure routing of interrupts to particular cpu cores
    pub fn set_interrupt_target(&self, int_id: u32, core: u32) {
        // We skip the banked registers since according to 2.2.1
        // those map to interrupt IDs 0-31 which are local to
        // the processor.
        //
        // According to 4.3.12:
        // - GICD_ITARGETSR0 to GICD_ITARGETSR7 are read-only
        // - GICD_ITARGETSR0 to GICD_ITARGETSR7 are banked
        if int_id < *Self::PPI_ID_RANGE.end() {
            return;
        }

        // Following the algorithm on page 4-107:
        // 1. ITARGETSR num = int_id / 4
        // minus 1 since we seperate banked registers
        let num = (int_id / 4) as usize - 1;
        // 2. byte offset required = int_id % 4
        let offset = int_id % 4;

        // change ITARGETSR
        //
        // Table 4-16: each bit in a CPU targets field refers to the corresponding processor
        let mut state = self.registers.ITARGETSR[num].get();
        state = state | (1 << core + offset * 8);
        self.registers.ITARGETSR[num].set(state);
    }

    /// configure the priority of an interrupt
    pub fn set_interrupt_priority(&self, int_id: u32, priority: u8) {
        // NOTE: for more info see 4.3.11

        // TODO: check the number of bits implemented

        // Following the algorithm on 4-105:
        // 1. GICD_IPRIORITYRn = int_id / 4
        let num = (int_id / 4) as usize;
        // 2. byte offset required = int_id % 4
        let offset = int_id % 4;

        // Each priority field holds a priority value.
        let prio = match offset {
            0 => IPRIORITYR::PriorityOffset0.val(priority.into()),
            1 => IPRIORITYR::PriorityOffset1.val(priority.into()),
            2 => IPRIORITYR::PriorityOffset2.val(priority.into()),
            3 => IPRIORITYR::PriorityOffset3.val(priority.into()),
            _ => unreachable!(),
        };

        self.registers.IPRIORITYR[num].modify(prio);
    }

    /// Send a software generated interrupt to a set of cores
    pub fn send_interrupt(&self, int_id: u32, dest: Destination) {
        // SGI's range from 0-15
        if int_id > *Self::SGI_ID_RANGE.end() {
            return;
        }

        // set target list filter and cpu list
        let (filter, targets) = match dest {
            Destination::Single(core) => {
                if current_processor().id == core {
                    // use optimized SGI path for ourselves
                    (SGIR::TargetListFilter::ToSelf, None)
                } else {
                    (
                        SGIR::TargetListFilter::ForwardSpecified,
                        Some(SGIR::CPUTargetList.val(1 << core)),
                    )
                }
            }
            Destination::AllButSelf => (SGIR::TargetListFilter::AllButSelf, None),
            Destination::Bsp | Destination::LowestPriority => {
                if current_processor().is_bsp() {
                    // use optimized SGI path for ourselves
                    (SGIR::TargetListFilter::ToSelf, None)
                } else {
                    (
                        SGIR::TargetListFilter::ForwardSpecified,
                        Some(SGIR::CPUTargetList.val(1 << current_processor().bsp_id())),
                    )
                }
            }
            _ => unimplemented!("unsupported SGI destination: {:?}", dest),
        };

        if let Some(target_list) = targets {
            // NOTE: forwarding of interrupts may have to be enabled (see 4.3.15)
            self.registers
                .SGIR
                .write(filter + target_list + SGIR::IntID.val(int_id));
        } else {
            self.registers.SGIR.write(filter + SGIR::IntID.val(int_id));
        }
    }

    /// Check if the interrupt is still pending.
    pub fn is_interrupt_pending(&self, int_id: u32, dest: Destination) -> bool {
        // TODO: support for PPI/SPI, GICD_ICPEND
        if !Self::SGI_ID_RANGE.contains(&int_id) {
            unimplemented!("unsupported interrupt number: {}", int_id)
        }

        // Each GICD_CPENDSGIRn register has 8 clear-pending bits
        // for four SGIs. 4 registers are implemented in total for
        // all 16 SGIs.
        //
        // Following the Algorithm on 4-116:
        // "For SGI ID x, generated by CPU C writing to its GICD_SGIR,"
        // The corresponding GICD_CPENDSGIRn = x / 4
        let num = (int_id / 4) as usize;
        // the SGI clear-pending field offset y = x % 4
        let offset = int_id % 4;

        // GICD_CPENDSGIRn provide a clear-pending bit for each
        // SGI and source processsor combination.
        let state = self.registers.CPENDSGIR[num].get();
        let sgi_status = state >> (offset * 8);

        match dest {
            Destination::Single(core) => Self::check_sgi_status(sgi_status, core),
            Destination::AllButSelf => {
                let current = current_processor();
                // NOTE: NR_CPUS is read only after bootstrap, so relaxed ordering is safe.
                let nr_cpus =
                    crate::processor::NR_CPUS.load(core::sync::atomic::Ordering::Relaxed) as u32;
                for core in 0..nr_cpus {
                    if core == current.id {
                        continue;
                    }
                    if Self::check_sgi_status(sgi_status, core) {
                        return true;
                    }
                }
                false
            }
            Destination::Bsp | Destination::LowestPriority => {
                let bsp = current_processor().bsp_id();
                Self::check_sgi_status(sgi_status, bsp)
            }
            _ => unimplemented!("unsupported SGI destination: {:?}", dest),
        }
    }

    // check if the specified SGI is still pending for a particular core
    fn check_sgi_status(sgi_status: u32, core_id: u32) -> bool {
        // the bit in the SGI x clear-pending field is bit C, for CPU C
        let bit = core_id;

        // GICD_CPENDSGIRn provide a clear-pending bit for each
        // SGI and source processsor combination. A write of 1
        // means the pending state is cleared. Reading a high bit
        // means that the SGI is pending. A read of 0 means that
        // the SGI is not pending.
        ((sgi_status >> bit) & 0x1) != 0
    }
}
