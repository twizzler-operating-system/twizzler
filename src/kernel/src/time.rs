use alloc::{
    sync::Arc,
    vec::Vec
};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

pub struct Ticks {
  pub value: u64,
  pub rate: FemtoSeconds
}

pub trait ClockHardware {
  fn read(&self) -> Ticks;
  fn info(&self) -> ClockInfo;
}

// struct DummySource;
// impl ClockHardware for DummySource {}
// register_clock(DummySource); // slots 0 and 1

pub static mut TICK_SOURCES: Vec<Arc<dyn ClockHardware>> = Vec::new();

pub fn register_clock<T: 'static + ClockHardware>(clock: T) {
  unsafe { TICK_SOURCES.push(Arc::new(clock)) }
}