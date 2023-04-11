/// Handling of external interrupt sources (e.g, IRQ).
///
/// External interrupt sources, or simply interrupts in
/// general orignate from a device or another processor
/// which can be routed by an interrupt controller
use twizzler_abi::{
    kso::{InterruptAllocateOptions, InterruptPriority},
    upcall::{UpcallFrame},
};

use crate::interrupt::{DynamicInterrupt, Destination};

use super::{
    // set_interrupt,
    thread::UpcallAble,
};

// interrupt vector table size/num vectors
pub const GENERIC_IPI_VECTOR: u32 = 0;
pub const MIN_VECTOR: usize = 0;
pub const MAX_VECTOR: usize = 0;
pub const RESV_VECTORS: &[usize] = &[0x0];
pub const NUM_VECTORS: usize = 0;

// interrupt service routine (isr) context
#[derive(Copy, Clone)]
#[repr(C)]
pub struct IsrContext;

impl UpcallAble for IsrContext {
    fn set_upcall(&mut self, _target: usize, _frame: u64, _info: u64, _stack: u64) {
        todo!("set_upcall for IsrContext")
    }

    fn get_stack_top(&self) -> u64 {
        todo!("get_stat for IsrContext")
    }
}

impl From<IsrContext> for UpcallFrame {
    fn from(_int: IsrContext) -> Self {
        todo!("conversion from IsrContext to UpcallFrame")
    }
}

impl core::fmt::Debug for IsrContext {
    fn fmt(&self, _f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        todo!("actually add debug info for IsrContext");
    }
}

// #[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn kernel_interrupt() {
    core::arch::asm!("nop", options(noreturn))
}

// #[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn user_interrupt() {
    core::arch::asm!("nop", options(noreturn))
}

// #[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn return_from_interrupt() {
    core::arch::asm!("nop", options(noreturn))
}

#[allow(unused_macros)] // DEBUG
// implement interrupt number specific interrupt handlers
macro_rules! interrupt {
    ($name:ident, $num:expr) => {
        #[naked]
        #[allow(named_asm_labels)]
        unsafe extern "C" fn $name() {
            /* TODO */
        }
    };
}

#[allow(unused_macros)] // DEBUG
// exception number specific interrupt error handlers
macro_rules! interrupt_err {
    ($name:ident, $num:expr) => {
        #[naked]
        #[allow(named_asm_labels)]
        unsafe extern "C" fn $name() {
            /* TODO */
        }
    };
}

// private interrupt descriptor table entry
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IDTEntry;

// private representation of interrupt descriptor table
#[repr(align(16), C)]
struct InterruptDescriptorTable;

// private representation of a pointer to the interrupt descriptor table
#[repr(C, packed)]
struct InterruptDescriptorTablePointer;

// private representation of exceptions
#[derive(Debug, Clone, Copy)]
#[repr(u64)]
enum Exception { 
    Unknown
}

// code for IPI signal to send 
pub enum InterProcessorInterrupt {
    Reschedule = 0, /* TODO */
}

// map handler function to a specific number into the interrupt descriptor table

// initialize arch-specific interrupt descriptor table
pub fn init_idt() {
    todo!()
}

bitflags::bitflags! {
    /// Interrupt mask bits for the DAIF register which changes PSTATE.
    pub struct DAIFMaskBits: u8 {
        /// D bit: Watchpoint, Breakpoint, and Software Step exceptions
        const DEBUG = 1 << 3;
        /// A bit: SError exceptions
        const SERROR = 1 << 2;
        /// I bit: IRQ exceptions
        const IRQ = 1 << 1;
        /// F bit: FIQ exceptions
        const FIQ = 1 << 0;
    }
}

pub fn disable() -> bool {
    unsafe {
        core::arch::asm!(
            "msr DAIFSet, {DISABLE_MASK}",
            DISABLE_MASK = const DAIFMaskBits::IRQ.bits(),
        );
    }
    // TODO: We need the current interrupt state,
    // for now we return true. Since the interrupt state
    // for aarch64 is more complex, maybe we need this
    // to be a type. Or we since we are only toggling a bit,
    // we could keep the bool representation
    true
}

pub fn set(_state: bool) {
    unsafe {
        core::arch::asm!(
            "msr DAIFClr, {ENABLE_MASK}",
           ENABLE_MASK = const DAIFMaskBits::IRQ.bits(),
        );
    }
}

pub fn allocate_interrupt_vector(
    _pri: InterruptPriority,
    _opts: InterruptAllocateOptions,
) -> Option<DynamicInterrupt> {
    // TODO: Actually track interrupts, and allocate based on priority and flags.
    todo!()
}

impl Drop for DynamicInterrupt {
    fn drop(&mut self) {
        // TODO
    }
}

pub fn send_ipi(_dest: Destination, _vector: u32) {
    todo!("send an ipi")
}
