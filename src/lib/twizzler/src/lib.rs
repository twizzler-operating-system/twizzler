#![feature(negative_impls)]
#![feature(rustc_attrs)]
#![feature(auto_traits)]
#![feature(allocator_api)]
#![allow(internal_features)]
#![feature(core_intrinsics)]

// This is required so we can use our derive macros in this crate.
extern crate self as twizzler;
pub use twizzler_derive::*;

pub mod alloc;
pub mod collections;
pub mod marker;
pub mod object;
pub mod ptr;
pub mod tx;

//mod pager;

#[cfg(test)]
mod tests;
