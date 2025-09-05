#![feature(negative_impls)]
#![feature(rustc_attrs)]
#![feature(auto_traits)]
#![feature(allocator_api)]
#![allow(internal_features)]
#![feature(core_intrinsics)]
#![feature(arbitrary_self_types)]
#![feature(backtrace_frames)]
#![feature(ptr_as_ref_unchecked)]
#![feature(test)]

// This is required so we can use our derive macros in this crate.
extern crate self as twizzler;
pub use twizzler_derive::*;

pub mod alloc;
pub mod collections;
pub mod marker;
pub mod object;
pub mod ptr;

pub(crate) mod util;

pub mod error {
    pub use twizzler_rt_abi::error::*;
}

pub use twizzler_rt_abi::Result;

//mod pager;

#[cfg(test)]
mod tests;