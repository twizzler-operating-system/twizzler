/// GICv2 Distributor Interface

use registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::{ReadOnly, ReadWrite},
};

use super::mmio::MmioRef;

use crate::memory::VirtAddr;

// Each register in the specification is prefixed with GICC_
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
        /// Interrupt Set-Enable Registers. ISENABLER0 is banked which
        /// holds the enable bits for each connected processor.
        (0x100 => ISENABLER_BANKED: ReadWrite<u32>),
        (0x104 => ISENABLER: [ReadWrite<u32>; 31]),
        (0x180 => _reserved2),
        // skip the banked ITARGETSR registers for now: int #'s 0-31
        (0x800 => ITARGETSR_BANKED: [ReadWrite<u32, ITARGETSR::Register>; 8]),
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
        let local_ints = self.registers.ISENABLER_BANKED.get();
        emerglogln!("\tISENABLER0: {:#x}", local_ints);
        // what is the interurpt mask for local interrupts routed?
        // a read of an of the cpu targets returns the number(core id) of the processor reading it
        let int_mask = self.registers.ITARGETSR_BANKED[0].read(ITARGETSR::TargetOffset0);
        emerglogln!("\tITARGETSR[0]: cpu number {:#x}", int_mask);
    }

    /// Set the enable bit for the corresponding interrupt.
    pub fn enable_interrupt(&self, int_id: u32) {
        match int_id {
            0..=31 => {
                // read register
                let mut enable = self.registers.ISENABLER_BANKED.get();
                let bit_index = int_id % 32;
                // set the local bit copy
                enable = enable | (1 << bit_index);
                // write out the value
                self.registers.ISENABLER_BANKED.set(enable);
            },
            _ => todo!("unsupported interrupt number: {}", int_id)
        }
    }

    /// configure routing of interrupts to particular cpu cores
    pub fn set_interrupt_target(&self, int_id: u32, _core: u32) {
        // change ITARGETSR
        match int_id {
            0..=31 => {
                // find banked index
                // find bit index
                // write the value to the register
            },
            _ => todo!("unsupported interrupt number: {}", int_id)
        }
    }
    
}
