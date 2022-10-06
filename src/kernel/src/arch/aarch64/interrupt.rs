use twizzler_abi::{
    kso::{InterruptAllocateOptions, InterruptPriority},
    upcall::{UpcallFrame},
};

use crate::interrupt::DynamicInterrupt;

use super::{
    // set_interrupt,
    thread::UpcallAble,
};

// interrupt vector table size/num vectors
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

#[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn kernel_interrupt() {
}

#[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn user_interrupt() {
}

#[allow(unsupported_naked_functions)] // DEBUG
#[allow(clippy::missing_safety_doc)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn return_from_interrupt() {
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

pub fn disable() -> bool {
    todo!("disable interrupts")
}

pub fn set(_state: bool) {
    todo!("enable interrupts")
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
