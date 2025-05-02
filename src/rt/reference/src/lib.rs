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
#![feature(btree_extract_if)]
#![feature(naked_functions)]
#![feature(c_variadic)]
#![feature(linkage)]
#![feature(c_size_t)]
#![feature(result_flattening)]

pub(crate) mod arch;

mod runtime;
pub use runtime::{set_upcall_handler, RuntimeState, OUR_RUNTIME};

mod error;
pub use error::*;

pub mod preinit;

#[allow(non_snake_case)]
pub mod syms;

pub mod pager;
