use core::sync::atomic::Ordering;

pub use address::{PhysAddr, VirtAddr};

use crate::{
    BootInfo,
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    thread::current_thread_ref,
};

pub mod acpi;
pub mod address;
mod apic;
pub mod context;
mod gdt;
pub mod image;
pub mod interrupt;
pub mod ioapic;
pub mod kvm;
pub mod memory;
mod pit;
pub mod processor;
mod start;
mod syscall;
pub mod thread;
mod tsc;
pub use apic::{poke_cpu, send_ipi};
pub use start::BootInfoSystemTable;

use self::apic::get_lapic;
pub fn init() {
    gdt::init();
    interrupt::init_idt();
    apic::init(true);
}

pub fn init_post_memory(boot_info: &dyn BootInfo) {
    let rsdp = boot_info.get_system_table(BootInfoSystemTable::Rsdp);
    acpi::init(rsdp.raw());
}

pub fn init_secondary() {
    gdt::init_secondary();
    interrupt::init_idt();
    apic::init(false);
    // Both call paths satisfy its preconditions: memory and cpu enumeration are done, and
    // CPU_ID/CURRENT_PROCESSOR tls is set (main.rs for the bsp, secondary_entry for aps).
    kvm::steal_time_cpu_init();
}

pub fn init_interrupts() {
    // Before the IOAPIC unmasks the ISA overrides: firmware leaves channel 0 running, and a tick
    // landing between the unmask and a later quiesce sits pending until interrupts come on.
    pit::quiesce();
    ioapic::init()
}

/// Per-cpu statclock state. The statclock used to be a PIT interrupt delivered to the bsp alone,
/// which re-broadcast it to every other cpu by IPI -- so profiling ticks cost n-1 IPIs each, and a
/// bsp that stopped taking interrupts silenced every other cpu's sampling (and largely their idle
/// wakeups) with it. Instead, each cpu keeps its own next-sample deadline and takes the sample
/// from its own LAPIC timer interrupt: [stat::clamp_oneshot] shortens any oneshot being programmed
/// so the timer fires by the deadline, and [stat::tick] runs the callback when it has passed.
/// Samples stay on the statclock's own cadence rather than snapping to hardtick boundaries, which
/// is the property the deliberately-off-beat statclock frequency exists for.
pub(super) mod stat {
    use core::sync::atomic::{AtomicU64, Ordering};

    use crate::{clock::Nanoseconds, once::Once};

    static PERIOD_NS: AtomicU64 = AtomicU64::new(0);
    static CB: Once<fn(Nanoseconds)> = Once::new();

    #[thread_local]
    static NEXT_NS: AtomicU64 = AtomicU64::new(0);
    #[thread_local]
    static LAST_NS: AtomicU64 = AtomicU64::new(0);

    pub(super) fn start(hz: u64, cb: fn(Nanoseconds)) {
        CB.call_once(|| cb);
        PERIOD_NS.store(1_000_000_000 / hz, Ordering::Release);
    }

    /// Floor on the delta [clamp_oneshot] can produce for an overdue sample: a delay of 0 would
    /// be written to TICR as 0 on the non-deadline path, which *stops* the timer.
    const MIN_DELTA_NS: Nanoseconds = 10_000;

    /// Shorten a oneshot delay so this cpu's timer fires by its next stattick deadline. Never
    /// lengthens: a caller asking for something sooner keeps it.
    pub(in crate::arch::amd64) fn clamp_oneshot(time: Nanoseconds) -> Nanoseconds {
        let period = PERIOD_NS.load(Ordering::Acquire);
        if period == 0 {
            return time;
        }
        let now = crate::instant::current_ns();
        if now == 0 {
            return time;
        }
        let mut next = NEXT_NS.load(Ordering::Relaxed);
        if next == 0 {
            // First arm on this cpu; start its cadence now.
            next = now + period;
            NEXT_NS.store(next, Ordering::Relaxed);
        }
        time.min(next.saturating_sub(now).max(MIN_DELTA_NS))
    }

    /// Run the statclock callback if this cpu's sample is due. Called from this cpu's LAPIC
    /// timer interrupt.
    pub(in crate::arch::amd64) fn tick() {
        let period = PERIOD_NS.load(Ordering::Acquire);
        if period == 0 {
            return;
        }
        let now = crate::instant::current_ns();
        if now == 0 || now < NEXT_NS.load(Ordering::Relaxed) {
            return;
        }
        // Advance from now, not the old deadline: a cpu that ran late owes one sample, not a
        // burst of catch-up samples that would all land on the same context.
        NEXT_NS.store(now + period, Ordering::Relaxed);
        let last = LAST_NS.swap(now, Ordering::Relaxed);
        let dt = if last == 0 { period } else { now - last };
        if let Some(cb) = CB.poll() {
            cb(dt);
        }
    }
}

pub fn start_clock(statclock_hz: u64, stat_cb: fn(Nanoseconds)) {
    // The PIT plays no part: it was quiesced in [init_interrupts], and the statclock runs on the
    // per-cpu LAPIC timers from here on.
    stat::start(statclock_hz, stat_cb);
}

pub fn schedule_oneshot_tick(time: Nanoseconds) {
    get_lapic().setup_oneshot_timer(stat::clamp_oneshot(time))
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(
    target: crate::memory::VirtAddr,
    stack: crate::memory::VirtAddr,
    arg: u64,
) {
    use crate::syscall::SyscallContext;
    let ctx = syscall::X86SyscallContext::create_jmp_context(target, stack, arg);
    crate::interrupt::set(false);
    crate::thread::exit_kernel();

    unsafe {
        {
            /* we need this scope the drop the current thread ref before returning to user */
            let user_fs = current_thread_ref()
                .unwrap()
                .arch
                .user_fs
                .load(Ordering::SeqCst);
            crate::arch::amd64::processor::write_fs_base(user_fs);
        }
        syscall::return_to_user(&ctx as *const syscall::X86SyscallContext);
    }
}

pub fn set_interrupt(
    num: u32,
    masked: bool,
    trigger: TriggerMode,
    polarity: PinPolarity,
    destination: Destination,
) {
    ioapic::set_interrupt(num - 32, num, masked, trigger, polarity, destination);
}

pub fn debug_shutdown(code: u32) {
    log::info!("performing debug shutdown with code {}", code);
    unsafe {
        x86::io::outw(0xf4, code as u16);
    }
}
