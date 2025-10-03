pub type DefaultHashBuilder = ahash::RandomState;

pub mod raw;
mod control;
mod benches;
pub mod map;

pub use map::{PersistentHashMap, PHMsession};