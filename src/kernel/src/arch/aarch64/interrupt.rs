/// Handling of external interrupt sources (e.g, IRQ).
///
/// External interrupt sources, or simply interrupts in
/// general orignate from a device or another processor
/// which can be routed by an interrupt controller
use arm64::registers::DAIF;
use registers::interfaces::Readable;
use twizzler_abi::kso::{InterruptAllocateOptions, InterruptPriority};

use super::{
    cntp::{cntp_interrupt_handler, PhysicalTimer},
    exception::{exception_handler, restore_stack_pointer, save_stack_pointer, ExceptionContext},
};
use crate::{
    interrupt::{Destination, DynamicInterrupt, PinPolarity, TriggerMode},
    machine::{
        interrupt::interrupt_controller,
        serial::{serial_int_id, serial_interrupt_handler},
    },
    processor::{ipi::generic_ipi_handler, mp::current_processor},
};

// Reserved SW-generated interrupt numbers.
// These numbers depend on the interrupt controller.
// We can and should dynamically allocate these,
// but this is fine for now.
pub const GENERIC_IPI_VECTOR: u32 = 0; // Used for IPI
pub const TLB_SHOOTDOWN_VECTOR: u32 = 1; // used for TLB consistency
pub const RESV_VECTORS: &[usize] = &[GENERIC_IPI_VECTOR as usize, TLB_SHOOTDOWN_VECTOR as usize];
// pub const TIMER_VECTOR: u32 = 3;

// IC controller specfific
// Used to interrupt generic code
pub use crate::machine::interrupt::{MAX_VECTOR, MIN_VECTOR, NUM_VECTORS};

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
    // Get pending IRQ number from GIC CPU Interface
    // and possibly return the core number that interrupted us.
    // Doing so acknowledges the pending interrupt.
    let (irq_number, sender_core) = interrupt_controller().pending_interrupt();

    match irq_number {
        PhysicalTimer::INTERRUPT_ID => {
            // call timer interrupt handler
            cntp_interrupt_handler();
        }
        _ if irq_number == serial_int_id() => {
            // call the serial interrupt handler
            serial_interrupt_handler();
        }
        GENERIC_IPI_VECTOR => {
            generic_ipi_handler();
        }
        _ => panic!("unknown irq number! {}", irq_number),
    }
    // signal the GIC that we have serviced the IRQ
    interrupt_controller().finish_active_interrupt(irq_number, sender_core);

    crate::interrupt::post_interrupt()
}

//----------------------------
//  interrupt controller APIs
//----------------------------
pub fn send_ipi(dest: Destination, vector: u32) {
    // tell the interrupt controller to send and interrupt
    interrupt_controller().send_interrupt(vector, dest);
    // wait while interrupt has not been recieved
    while interrupt_controller().is_interrupt_pending(vector, dest) {
        core::hint::spin_loop();
    }
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
    Reschedule = 2, /* TODO */
}

impl Drop for DynamicInterrupt {
    fn drop(&mut self) {
        // TODO
    }
}

pub fn init_interrupts() {
    let cpu = current_processor();

    emerglogln!(
        "[arch::interrupt] processor {} initializing interrupts",
        cpu.id
    );

    // initialize interrupt controller
    if cpu.is_bsp() {
        interrupt_controller().configure_global();
    }
    interrupt_controller().configure_local();

    // enable this CPU to recieve interrupts from the timer
    // by configuring the interrupt controller to route
    // the timer's interrupt to us
    interrupt_controller().route_interrupt(PhysicalTimer::INTERRUPT_ID, cpu.id);
    interrupt_controller().enable_interrupt(PhysicalTimer::INTERRUPT_ID);
}

pub fn set_interrupt(
    num: u32,
    _masked: bool,
    _trigger: TriggerMode,
    _polarity: PinPolarity,
    destination: Destination,
) {
    match destination {
        Destination::Bsp => {
            interrupt_controller().route_interrupt(num, current_processor().bsp_id())
        }
        _ => todo!("routing interrupt: {:?}", destination),
    }
    interrupt_controller().enable_interrupt(num);
}
