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
pub use interrupt::{send_ipi, init_interrupts};
pub use start::BootInfoSystemTable;

pub fn init<B: BootInfo>(_boot_info: &B) {
    logln!("[arch::init] initializing exceptions");
    exception::init();

    #[cfg(test)]
    {
        // intialize an instance of the timer
        processor::enumerate_clocks();

        use crate::time::TICK_SOURCES;
        use twizzler_abi::syscall::{
            NanoSeconds, ClockSource, NANOS_PER_SEC, FEMTOS_PER_SEC, TimeSpan,
        };

        let clk_src: u64 = ClockSource::BestMonotonic.into();
        let cntp = &TICK_SOURCES.lock()[clk_src as usize];

        let info = cntp.info();
        logln!("[arch::timer] frequency: {} Hz, {} fs, {} ns", 
            FEMTOS_PER_SEC / info.resolution().0, info.resolution().0, {
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

        init_interrupts();

        // set the timer interrupt to be routed to us
        crate::machine::interrupt::INTERRUPT_CONTROLLER
            .enable_interrupt(cntp::PhysicalTimer::INTERRUPT_ID);

        // configure the timer to fire off interrupts
        const WAIT_TIME: TimeSpan = TimeSpan::from_secs(3);

        let phys_timer = cntp::PhysicalTimer::new();
        phys_timer.set_timer(WAIT_TIME);

        logln!("[arch::timer] firing off an interrupt in {} seconds...", WAIT_TIME.0.0);

        logln!("[kernel::arch] looping forever");
        let mut count = 0;
        let mut tick0 = cntp.read();
        loop {
            // record start timestamp
            let start_time = tick0.value * tick0.rate;
            let mut tick1;
            let mut end_time;
            // check if half a second passed
            loop {
                tick1 = cntp.read();
                end_time = tick1.value * tick1.rate;
                if (end_time - start_time).as_nanos() >= (NANOS_PER_SEC / 2).into() {
                    // reset starting timestamp
                    tick0 = cntp.read();
                    // increment half second counts
                    count += 1;
                    logln!("[kernel:test] 1/2 second has passed: {}", count);
                    break
                }
            }
            // if 6 seconds have passed
            if count >= 12 {
                // reset the interrupt timer count to 5 seconds later
                logln!("[kernel::test] setting timer to fire in 5 seconds");
                phys_timer.set_timer(TimeSpan::from_secs(5));

                count = 0;
            }
        }
    }
}

pub fn init_secondary() {
    todo!();
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
