#![feature(core_intrinsics)]
#![feature(thread_local)]

pub(crate) mod arch;

mod runtime;

mod error;
pub use error::*;

pub(crate) mod preinit;
