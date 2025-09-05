pub type DefaultHashBuilder = foldhash::fast::RandomState;

pub mod raw;
mod scopeguard;
mod control;
mod benches;
pub mod map;

pub use map::PersistentHashMap;