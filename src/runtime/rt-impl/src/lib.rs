//! # The Twizzler Reference Runtime
//!
//! This is a work in progress.

#![allow(internal_features)]
#![feature(core_intrinsics)]
#![feature(thread_local)]
#![feature(fmt_internals)]
#![feature(array_windows)]
#![feature(unboxed_closures)]
#![feature(allocator_api)]
#![feature(hash_extract_if)]
#![feature(btree_extract_if)]
#![feature(naked_functions)]
#![feature(c_variadic)]
#![feature(linkage)]

pub(crate) mod arch;

mod runtime;
pub use runtime::{set_upcall_handler, RuntimeState, OUR_RUNTIME};

mod error;
pub use error::*;

pub mod preinit;

pub mod syms;
