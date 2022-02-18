use crate::arch::{self, interrupt::InterProcessorInterrupt};

#[inline]
pub fn disable() -> bool {
    crate::arch::interrupt::disable()
}

#[inline]
pub fn set(state: bool) {
    crate::arch::interrupt::set(state);
}

#[inline]
pub fn with_disabled<T, F: FnOnce() -> T>(f: F) -> T {
    let tmp = disable();
    let t = f();
    set(tmp);
    t
}

#[inline]
pub fn post_interrupt() {
    crate::sched::schedule_maybe_preempt();
}

#[inline]
pub fn send_ipi(destination: Destination, ipi: InterProcessorInterrupt) {
    arch::lapic::send_ipi(destination, ipi as u32)
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

pub fn external_interrupt_entry(_number: u32) {
    //logln!("external device interrupt {}", number);
}
