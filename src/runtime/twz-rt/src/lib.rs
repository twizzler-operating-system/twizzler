#![feature(core_intrinsics)]

pub(crate) mod arch;

mod runtime;

mod error;
pub use error::*;

pub(crate) mod preinit;
