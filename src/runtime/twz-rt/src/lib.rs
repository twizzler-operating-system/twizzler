//! # The Twizzler Reference Runtime
//! The Reference Runtime implements the Runtime trait from twizzler-runtime-abi, and is designed to
//! be the primary, fully supported programming environment on Twizzler.
//!
//! This is a work in progress.

#![feature(core_intrinsics)]
#![feature(thread_local)]
#![feature(fmt_internals)]
#![feature(array_windows)]
#![feature(unboxed_closures)]
#![feature(allocator_api)]
#![feature(hash_extract_if)]

pub(crate) mod arch;

pub use arch::rr_upcall_entry;

mod runtime;
pub use runtime::{
    set_upcall_handler, CompartmentInitInfo, RuntimeState, RuntimeThreadControl, OUR_RUNTIME,
};

mod error;
pub use error::*;

pub mod preinit;
