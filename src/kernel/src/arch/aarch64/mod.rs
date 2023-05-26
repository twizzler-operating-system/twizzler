use crate::{
    clock::Nanoseconds,
    interrupt::{Destination, PinPolarity, TriggerMode},
    BootInfo,
};

pub mod address;
mod cntp;
pub mod context;
mod exception;
pub mod interrupt;
pub mod memory;
pub mod processor;
mod syscall;
pub mod thread;
mod start;

pub use address::{VirtAddr, PhysAddr};
pub use interrupt::send_ipi;
pub use start::BootInfoSystemTable;

pub fn init<B: BootInfo>(_boot_info: &B) {
    logln!("[arch::init] initializing exceptions");
    exception::init();

    // intialize an instance of the timer
    processor::enumerate_clocks();

    use crate::time::TICK_SOURCES;
    use twizzler_abi::syscall::{NanoSeconds, ClockSource};

    let clk_src: u64 = ClockSource::BestMonotonic.into();
    let cntp = &TICK_SOURCES.lock()[clk_src as usize];

    let info = cntp.info();
    logln!("[arch::timer] frequency: {} Hz, {} fs, {} ns", 
        1_000_000_000_000_000 / info.resolution().0, info.resolution().0, {
            let femtos = info.resolution();
            let nanos: NanoSeconds = femtos.into();
            nanos.0
        }
    );

    // read the timer
    let t = cntp.read();
    logln!("[arch::timer] current timer count: {}, uptime: {:?}",
        t.value, t.value * t.rate
    );
}

pub fn init_secondary() {
    todo!();
}

pub fn init_interrupts() {
    todo!()
}

pub fn set_interrupt(
    _num: u32,
    _masked: bool,
    _trigger: TriggerMode,
    _polarity: PinPolarity,
    _destination: Destination,
) {
    todo!();
}

pub fn start_clock(_statclock_hz: u64, _stat_cb: fn(Nanoseconds)) {
    todo!();
}

pub fn schedule_oneshot_tick(_time: Nanoseconds) {
    todo!()
}

/// Jump into userspace
/// # Safety
/// The stack and target must be valid addresses.
pub unsafe fn jump_to_user(_target: crate::memory::VirtAddr, _stack: crate::memory::VirtAddr, _arg: u64) {
    todo!();
}

pub fn debug_shutdown(_code: u32) {
    todo!()
}

/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(_cpu: u32, _tcb_base: crate::memory::VirtAddr, _kernel_stack: *mut u8) {
    todo!("start up a cpu")
}
