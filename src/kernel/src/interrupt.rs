use crate::arch::{self, interrupt::InterProcessorInterrupt};

pub fn disable() -> bool {
    crate::arch::interrupt::disable()
}

pub fn set(state: bool) {
    crate::arch::interrupt::set(state);
}

pub fn with_disabled<T, F: FnOnce() -> T>(f: F) -> T {
    let tmp = disable();
    let t = f();
    set(tmp);
    t
}

pub fn post_interrupt() {
    crate::sched::schedule_maybe_preempt();
}

pub fn send_ipi(destination: Destination, ipi: InterProcessorInterrupt) {
    unsafe { arch::lapic::send_ipi(destination, ipi as u32) }
}

#[derive(Debug, Clone, Copy)]
pub enum PinPolarity {
    ActiveHigh,
    ActiveLow,
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerMode {
    Edge,
    Level,
}

#[derive(Debug, Clone, Copy)]
pub enum Destination {
    Bsp,
    Single(u32),
    LowestPriority,
    AllButSelf,
    All,
}
