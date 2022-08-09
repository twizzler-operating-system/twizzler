use alloc::vec::Vec;

use crate::{
    arch::{
        self,
        interrupt::{InterProcessorInterrupt, NUM_VECTORS},
    },
    obj::ObjectRef,
    once::Once,
    spinlock::Spinlock,
};

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

pub struct WakeInfo {
    obj: ObjectRef,
    offset: usize,
}

impl WakeInfo {
    pub fn wake(&self, val: u64) {
        unsafe {
            self.obj.write_val_and_signal(self.offset, val, usize::MAX);
        }
    }

    pub fn new(obj: ObjectRef, offset: usize) -> Self {
        Self { obj, offset }
    }
}

struct InterruptInner {
    target: Vec<WakeInfo>,
}

impl InterruptInner {
    pub fn raise(&self, val: u64) {
        for wi in &self.target {
            wi.wake(val)
        }
    }
}
struct Interrupt {
    inner: Spinlock<InterruptInner>,
    vector: usize,
}

impl Interrupt {
    pub fn raise(&self) {
        self.inner.lock().raise(self.vector as u64);
    }

    fn add(&self, wi: WakeInfo) {
        self.inner.lock().target.push(wi)
    }

    fn new(vector: usize) -> Self {
        Self {
            inner: Spinlock::new(InterruptInner { target: Vec::new() }),
            vector,
        }
    }
}

struct GlobalInterruptState {
    ints: Vec<Interrupt>,
}

static GLOBAL_INT: Once<GlobalInterruptState> = Once::new();
fn get_global_interrupts() -> &'static GlobalInterruptState {
    let mut v = Vec::new();
    for i in 0..NUM_VECTORS {
        v.push(Interrupt::new(i));
    }
    GLOBAL_INT.call_once(|| GlobalInterruptState { ints: v })
}

pub fn set_userspace_interrupt_wakeup(number: u32, wi: WakeInfo) {
    let gi = get_global_interrupts();
    gi.ints[number as usize].add(wi);
}

pub fn external_interrupt_entry(number: u32) {
    let gi = get_global_interrupts();
    gi.ints[number as usize].raise();
    //logln!("external device interrupt {}", number);
}
