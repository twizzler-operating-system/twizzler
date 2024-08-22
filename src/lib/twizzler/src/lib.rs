#![feature(allocator_api)]
#![feature(auto_traits)]
#![feature(negative_impls)]
#![feature(strict_provenance)]
#![feature(core_intrinsics)]
#![feature(thread_local)]
#![feature(test)]

extern crate self as twizzler;

pub mod alloc;
pub mod collections;
pub mod marker;
pub mod object;
pub mod ptr;
pub mod tx;

mod tests;

pub use twizzler_derive::Invariant;
