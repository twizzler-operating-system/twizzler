/// Handling of external interrupt sources (e.g, IRQ).
///
/// External interrupt sources, or simply interrupts in
/// general orignate from a device or another processor
/// which can be routed by an interrupt controller

use arm64::registers::DAIF;
use registers::interfaces::Readable;

use twizzler_abi::{
    kso::{InterruptAllocateOptions, InterruptPriority},
};

use crate::interrupt::{DynamicInterrupt, Destination};
use crate::machine::interrupt::INTERRUPT_CONTROLLER;

use super::exception::{
    ExceptionContext, exception_handler, save_stack_pointer, restore_stack_pointer,
};
use super::cntp::{PhysicalTimer, cntp_interrupt_handler};

// interrupt vector table size/num vectors
pub const GENERIC_IPI_VECTOR: u32 = 0; // Used for IPI
pub const MIN_VECTOR: usize = 0;
pub const MAX_VECTOR: usize = 0;
pub const RESV_VECTORS: &[usize] = &[0x0];
pub const NUM_VECTORS: usize = 0; // Used to interrupt generic code

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

/// Set the current interrupt enable state to disabled and return the old state.
pub fn disable() -> bool {
    // check if interrutps were already enabled.
    // if the I bit is set, then IRQ exceptions
    // are already masked
    let irq_enabled = !DAIF.is_set(DAIF::I);

    // if interrupts were not masked
    if irq_enabled {
        // disable interrupts
        unsafe {
            core::arch::asm!(
                "msr DAIFSet, {DISABLE_MASK}",
                DISABLE_MASK = const DAIFMaskBits::IRQ.bits(),
            );
        }
    }
    // return IRQ state to the caller
    irq_enabled
}

/// Set the current interrupt enable state.
pub fn set(state: bool) {
    // state singifies if interrupts need to enabled or disabled
    // the state can refer to the previous state of the I bit (IRQ)
    // in DAIF or may be explicitly changed. we unmask (enable) interrupts
    // if the state is true, and disable if false.
    if state {
        // enable interrupts by unmasking the I bit (the same as state)
        unsafe {
            core::arch::asm!(
                "msr DAIFClr, {ENABLE_MASK}",
                ENABLE_MASK = const DAIFMaskBits::IRQ.bits(),
            );
        }
    } else {
        disable();
    }
}

/// Get the current interrupt enable state without modifying it.
pub fn get() -> bool {
    // if the I bit is set, then IRQ exceptions are masked (disabled)
    // We return false for masked interrupts and true for
    // unmasked (enabled) interrupts
    !DAIF.is_set(DAIF::I)
}

// The top level interrupt request (IRQ) handler. Deals with
// interacting with the interrupt controller and acknowledging
// device interrupts.
exception_handler!(interrupt_request_handler_el1, irq_exception_handler, true);
exception_handler!(interrupt_request_handler_el0, irq_exception_handler, false);

/// Exception handler manages IRQs and calls the appropriate
/// handler for a given IRQ number. This handler manages state
/// in the interrupt controller.
pub(super) fn irq_exception_handler(_ctx: &mut ExceptionContext) {
    // TODO: for compatability, ARM recommends reading the entire
    // GICC_IAR register and writing that to GICC_EOIR

    // Get pending IRQ number from GIC CPU Interface.
    // Doing so acknowledges the pending interrupt.
    let irq_number = INTERRUPT_CONTROLLER.pending_interrupt();
    // emerglogln!("[arch::irq] interrupt: {}", irq_number);
    
    match irq_number {
        PhysicalTimer::INTERRUPT_ID => {
            // call timer interrupt handler
            cntp_interrupt_handler();
        },
        _ => panic!("unknown reason!")
    }
    // signal the GIC that we have serviced the IRQ
    INTERRUPT_CONTROLLER.finish_active_interrupt(irq_number);

    crate::interrupt::post_interrupt()
}

//----------------------------
//  interrupt controller APIs
//----------------------------
pub fn send_ipi(_dest: Destination, _vector: u32) {
    todo!("send an ipi")
}

// like register, used by generic code
pub fn allocate_interrupt_vector(
    _pri: InterruptPriority,
    _opts: InterruptAllocateOptions,
) -> Option<DynamicInterrupt> {
    // TODO: Actually track interrupts, and allocate based on priority and flags.
    todo!()
}

// code for IPI signal to send 
// needed by generic IPI code
pub enum InterProcessorInterrupt {
    Reschedule = 0, /* TODO */
}

impl Drop for DynamicInterrupt {
    fn drop(&mut self) {
        // TODO
    }
}

pub fn init_interrupts() {
    // we don't want to use logln since it enables interrupts
    // in the future we should not use logging until mm us up
    emerglogln!("[arch::interrupt] initializing interrupts");
    
    // initialize interrupt controller
    INTERRUPT_CONTROLLER.configure();

    // enable this CPU to recieve interrupts from the timer
    // by configuring the interrupt controller to route
    // the timer's interrupt to us
    INTERRUPT_CONTROLLER.enable_interrupt(PhysicalTimer::INTERRUPT_ID);
}

// in crate::arch::aarch64
// pub fn set_interrupt(
//     _num: u32,
//     _masked: bool,
//     _trigger: TriggerMode,
//     _polarity: PinPolarity,
//     _destination: Destination,
// ) {
//     todo!();
// }
