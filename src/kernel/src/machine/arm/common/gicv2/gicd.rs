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
    interfaces::{Readable, Writeable, ReadWriteable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

use super::super::mmio::MmioRef;

use crate::memory::VirtAddr;

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
    ]
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
        (0x820 => ITARGETSR: [ReadWrite<u32, ITARGETSR::Register>; 248]),
        (0xC00 => @END),
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
    const SGI_ID_RANGE: RangeInclusive<u32> = RangeInclusive::new(0, 15);

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
            return
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
            _ => unreachable!()
        };

        self.registers.IPRIORITYR[num].modify(prio);
    }
}
