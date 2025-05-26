#![feature(test)]
#![no_std]
//NOTE: temporary, remove later
#![allow(dead_code)]

extern crate alloc;

#[cfg(all(feature = "kernel", feature = "user"))]
compile_error!("feature \"kernel\" and feature \"user\" cannot be enabled at the same time");

pub(crate) use twizzler_rt_abi::error::SecurityError;

#[cfg(feature = "user")]
mod benches;

mod capability;
mod delegation;
mod flags;
mod gates;
mod keys;
mod revocation;
mod sec_ctx;

pub use capability::*;
pub use delegation::*;
pub use flags::*;
pub use gates::*;
pub use keys::*;
pub use revocation::*;
pub use sec_ctx::*;
