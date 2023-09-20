/// A Generic Interrupt Controller (GIC) v2 CPU Interface
/// 
/// The full specification can be found here:
///     https://developer.arm.com/documentation/ihi0048/b?lang=en
///
/// Relevant sections include, but are not limited to: 2.3, 4.1.3, 4.3
///
/// A summary of its functionality can be found in section 10.6
/// "ARM Cortex-A Series Programmer’s Guide for ARMv8-A":
///     https://developer.arm.com/documentation/den0024/a/

use registers::{
    interfaces::{Readable, Writeable},
    register_bitfields, register_structs,
    registers::ReadWrite,
};

use super::super::mmio::MmioRef;

use crate::memory::VirtAddr;

// Each register in the specification is prefixed with GICC_
register_bitfields! {
    u32,

    /// CPU Interface Control Register
    CTLR [
        // bits [31:1] are reserved
        Enable OFFSET(0) NUMBITS(1) []
    ],

    /// Interrupt Priority Mask Register
    PMR [
        // bits [8:31] are reserved
        Priority OFFSET(0) NUMBITS(8) []
    ],

    /// Interrupt Acknowledge Register
    IAR [
        // bits [12:10] are used to identify the CPUID of an SGI
        InterruptID OFFSET(0) NUMBITS(10) []
    ],

    /// End of Interrupt Register
    EOIR [
        // the EOIINTID field value
        InterruptID OFFSET(0) NUMBITS(10) []
    ]
}

// Each register in the specification is prefixed with GICC_
register_structs! {
    #[allow(non_snake_case)]
    pub CpuInterfaceRegisters {
        (0x000 => CTLR: ReadWrite<u32, CTLR::Register>),
        (0x004 => PMR: ReadWrite<u32, PMR::Register>),
        (0x008 => _reserved1),
        (0x00C => IAR: ReadWrite<u32, IAR::Register>),
        (0x010 => EOIR: ReadWrite<u32, EOIR::Register>),
        (0x014  => @END),
    }
}

/// GIC CPU Interface
pub struct GICC {
    registers: MmioRef<CpuInterfaceRegisters>,
}

impl GICC {
    pub const ACCEPT_ALL: u8 = 255;

    pub fn new(base: VirtAddr) -> Self {
        Self {
            registers: MmioRef::new(base.as_ptr::<CpuInterfaceRegisters>()),
        }
    }

    /// enable the cpu interface
    pub fn enable(&self) {
        // enables interrupts to signal the cpu interface of the processor
        self.registers.CTLR.write(CTLR::Enable::SET);
    }

    /// set a threshold for the interrupts that we will be signaled by
    pub fn set_interrupt_priority_mask(&self, mask: u8) {
        // A higher priority corresponds to a lower priority field value
        // A value of zero means that we mask all interrutps to the current processor
        self.registers.PMR.write(PMR::Priority.val(mask as u32));
    }

    /// get the interrupt id for a pending interrupt signal
    pub fn get_pending_interrupt_number(&self) -> u32 {
        // Reading the interrupt id causes the interrupt
        // to be marked active in the distributor. This
        // returns the interrupt id of the highest pending interrupt.
        // The register could return a spurious interrupt id
        // with a value of 1023
        let int_id = self.registers.IAR.read(IAR::InterruptID);
        if int_id == 1023 {
            // spurious interrupt only occurs if:
            // - forwarding from distributor to cpu is disabled
            // - signaling by cpu interface to processor is disabled
            // - all pending interrupts are low priority
            panic!("spurious interrupt id detected!!");
        }
        // every read of the IAR must have a matching write to the EOIR
        int_id.into()
    }

    /// notify cpu interface that processing of interrupt has completed
    pub fn finish_active_interrupt(&self, int_id: u32) {
        // A write to the EOIR corrsponds to the most recent valid
        // read of the IAR value. A return of a spurious ID from IAR
        // does not have to be written to EOIR.
        self.registers.EOIR.write(EOIR::InterruptID.val(int_id as u32));
    }

    /// print the configuration of the distributor
    pub fn print_config(&self) {
        emerglogln!("[gic::gicc] printing configuration");
        // is it enabled?
        let enabled = self.registers.CTLR.read(CTLR::Enable);
        emerglogln!("\tCTLR::Enable: {}", enabled);
        // what is the priority mask?
        let mask = self.registers.PMR.read(PMR::Priority);
        emerglogln!("\tPMR::Priority: {}", mask);
    }
}
