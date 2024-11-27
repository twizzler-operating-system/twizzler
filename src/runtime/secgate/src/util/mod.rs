//! A set of utility types for low-level communication between compartments.

mod buffer;
mod handle;

pub use buffer::*;
pub use handle::*;

#[cfg(test)]
extern crate twizzler_minruntime;
