pub type DefaultHashBuilder = foldhash::fast::RandomState;

mod raw;
mod scopeguard;
mod control;

pub mod map;
pub mod set;
pub mod table;
