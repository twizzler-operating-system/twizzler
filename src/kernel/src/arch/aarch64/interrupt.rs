use core::sync::atomic::{AtomicU64, Ordering};

/// Handling of external interrupt sources (e.g, IRQ).
///
/// External interrupt sources, or simply interrupts in
/// general orignate from a device or another processor
/// which can be routed by an interrupt controller
use arm64::registers::DAIF;
use registers::interfaces::Readable;
use twizzler_abi::kso::{InterruptAllocateOptions, InterruptPriority};

use super::{
    cntp::{PhysicalTimer, cntp_interrupt_handler},
    exception::{ExceptionContext, exception_handler, restore_stack_pointer, save_stack_pointer},
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
pub const RESCHED_IPI_VECTOR: u32 = 2;
pub const RESV_VECTORS: &[usize] = &[
    GENERIC_IPI_VECTOR as usize,
    TLB_SHOOTDOWN_VECTOR as usize,
    RESCHED_IPI_VECTOR as usize,
];
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
pub(super) fn irq_exception_handler(ctx: &mut ExceptionContext) {
    // As in `sync_handler`: an interrupt from EL0 is a user entry, and an upcall queued while
    // handling it (a sample, a suspend) needs these registers.
    let from_user = ctx.spsr & 0xf == 0;
    if from_user {
        if let Some(t) = crate::thread::current_thread_ref() {
            t.set_entry_registers(Some(ctx as *mut ExceptionContext));
        }
    } else {
        super::exception::check_kernel_stack(ctx.sp);
    }
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
        RESCHED_IPI_VECTOR => crate::processor::sched::schedule_resched(),
        _ => crate::interrupt::external_interrupt_entry(irq_number),
    }
    // signal the GIC that we have serviced the IRQ
    interrupt_controller().finish_active_interrupt(irq_number, sender_core);

    crate::interrupt::count_interrupt();
    crate::interrupt::post_interrupt();

    if from_user && let Some(t) = crate::thread::current_thread_ref() {
        t.set_entry_registers(None);
    }
}

//----------------------------
//  interrupt controller APIs
//----------------------------
pub fn send_ipi(dest: Destination, vector: u32) {
    // No wait: GICD_CPENDSGIR is banked per *receiver*, so the sender cannot observe delivery,
    // and spinning on its own bank with interrupts masked wedged on any SGI sent to it.
    interrupt_controller().send_interrupt(vector, dest);
}

/// One bit per SPI of the GICv2m frame, set while allocated.
static MSI_USED: [AtomicU64; MSI_WORDS] = [const { AtomicU64::new(0) }; MSI_WORDS];
// The frame's MSI_TYPER field is 10 bits wide.
const MSI_WORDS: usize = 1024 / 64;

// like register, used by generic code
pub fn allocate_interrupt_vector(
    _pri: InterruptPriority,
    _opts: InterruptAllocateOptions,
    destination: Destination,
) -> Option<DynamicInterrupt> {
    // GICv2 names at most 8 cpu interfaces; `set_interrupt` asserts it.
    if let Destination::Single(id) = destination
        && id >= 8
    {
        return None;
    }
    // MSI only: one SPI from the GICv2m frame's range.
    let (_, base, count) = crate::machine::interrupt::msi_frame()?;
    let i = (0..count as usize).find(|&i| {
        let bit = 1 << (i % 64);
        MSI_USED[i / 64].fetch_or(bit, Ordering::SeqCst) & bit == 0
    })?;
    let spi = base + i as u32;
    set_interrupt(
        spi,
        false,
        TriggerMode::Edge,
        PinPolarity::ActiveHigh,
        destination,
    );
    Some(DynamicInterrupt::new(spi as usize))
}

// code for IPI signal to send
// needed by generic IPI code
pub enum InterProcessorInterrupt {
    Reschedule = RESCHED_IPI_VECTOR as isize,
}

impl Drop for DynamicInterrupt {
    fn drop(&mut self) {
        let Some((_, base, count)) = crate::machine::interrupt::msi_frame() else {
            return;
        };
        let spi = self.num() as u32;
        if !(base..base + count).contains(&spi) {
            return;
        }
        interrupt_controller().disable_interrupt(spi);
        let i = (spi - base) as usize;
        MSI_USED[i / 64].fetch_and(!(1 << (i % 64)), Ordering::SeqCst);
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
    trigger: TriggerMode,
    _polarity: PinPolarity,
    destination: Destination,
) {
    interrupt_controller().set_edge_triggered(num, matches!(trigger, TriggerMode::Edge));
    // GICv2 names at most 8 cpu interfaces, one target bit each.
    let bit = |id: u32| {
        1u8.checked_shl(id)
            .expect("cpu id beyond GICv2's 8 targets")
    };
    let cpus = |skip: Option<u32>| {
        let mut mask = 0u8;
        crate::processor::mp::with_each_active_processor(|p| {
            if Some(p.id) != skip {
                mask |= bit(p.id);
            }
        });
        mask
    };
    // A GICv2 SPI with several targets is delivered to one of them (the 1-N model), which is the
    // closest this controller comes to LowestPriority.
    let targets = match destination {
        Destination::Bsp => bit(current_processor().bsp_id()),
        Destination::Single(id) => bit(id),
        Destination::LowestPriority | Destination::All => cpus(None),
        Destination::AllButSelf => cpus(Some(current_processor().id)),
    };
    interrupt_controller().route_interrupt_to(num, targets);
    interrupt_controller().enable_interrupt(num);
}
