#[allow(unused_imports)]
use crate::upcall::{UpcallData, UpcallInfo};

/// Arch-specific frame info for upcall.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct UpcallFrame {
    // general purpose registers
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,

    // callee-saved registers
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,

    /// link register
    pub x29: u64,
    /// frame pointer (i.e., x30)
    pub fp: u64,
    /// The stack pointer, depending on the context where the exception
    /// occurred, this is either sp_el0 or sp_el1
    pub sp: u64,
    /// The program counter. The address where the exception occurred (i.e., ELR_EL1)
    pub pc: u64,
    /// The state of the processor (SPSR_EL1). Determines execution environment (e.g., interrupts)
    pub spsr: u64,
    // Thread local storage for user space
    pub tpidr: u64,
    pub tpidrro: u64,

    // security context
    pub prior_ctx: crate::object::ObjID,

    // FP/SIMD state: the kernel never touches it, so it is live in the registers at delivery.
    pub fpcr: u64,
    pub fpsr: u64,
    pub v: [u128; 32],
}

impl UpcallFrame {
    /// Get the instruction pointer of the frame.
    pub fn ip(&self) -> usize {
        self.pc as usize
    }

    /// Get the stack pointer of the frame.
    pub fn sp(&self) -> usize {
        self.sp as usize
    }

    /// Get the base pointer of the frame.
    pub fn bp(&self) -> usize {
        self.fp as usize
    }

    /// Build a new frame set up to enter a context at a start point.
    pub fn new_entry_frame(
        stack_base: usize,
        stack_size: usize,
        tp: usize,
        ctx: crate::object::ObjID,
        entry: usize,
        arg: usize,
    ) -> Self {
        Self {
            x0: arg as u64,
            x1: 0,
            x2: 0,
            x3: 0,
            x4: 0,
            x5: 0,
            x6: 0,
            x7: 0,
            x8: 0,
            x9: 0,
            x10: 0,
            x11: 0,
            x12: 0,
            x13: 0,
            x14: 0,
            x15: 0,
            x16: 0,
            x17: 0,
            x18: 0,
            x19: 0,
            x20: 0,
            x21: 0,
            x22: 0,
            x23: 0,
            x24: 0,
            x25: 0,
            x26: 0,
            x27: 0,
            x28: 0,
            x29: 0,
            fp: 0,
            sp: ((stack_base + stack_size) & !0xf) as u64,
            pc: entry as u64,
            // EL0t, IRQs on, D/A/F masked: what the kernel itself uses to enter EL0.
            spsr: 0x340,
            tpidr: tp as u64,
            tpidrro: 0,
            prior_ctx: ctx,
            fpcr: 0,
            fpsr: 0,
            v: [0; 32],
        }
    }
}
