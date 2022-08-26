use alloc::{
    sync::Arc,
    vec::Vec
};

use twizzler_abi::syscall::{ClockInfo, FemtoSeconds};

use crate::spinlock::Spinlock;

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

pub static TICK_SOURCES: Spinlock<Vec<Arc<dyn ClockHardware + Send + Sync>>> = Spinlock::new(Vec::new());

pub fn register_clock<T>(clock: T)
where
  T: 'static + ClockHardware + Send + Sync,
{
  TICK_SOURCES.lock().push(Arc::new(clock))
}