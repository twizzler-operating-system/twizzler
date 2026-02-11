#![feature(test)]
#![no_std]
#![warn(missing_debug_implementations, missing_docs)]

//! Security primitives and capabilities for Twizzler.
//!
//! This crate provides the core security infrastructure including capabilities,
//! delegations, gates, and security contexts.
//!
//! # Features
//!
//! - `kernel` - Enable kernel-space functionality
//! - `user` - Enable user-space functionality (mutually exclusive with `kernel`)
//!
//! ```

extern crate alloc;

#[cfg(all(feature = "kernel", feature = "user"))]
compile_error!("feature \"kernel\" and feature \"user\" cannot be enabled at the same time");

pub(crate) use twizzler_rt_abi::error::SecurityError;

#[cfg(feature = "user")]
mod benches;

#[cfg(feature = "user")]
mod builder_ext;
mod capability;
mod delegation;
mod flags;
mod gates;
mod keys;
mod revocation;
mod sec_ctx;

#[cfg(feature = "user")]
pub use builder_ext::*;
pub use capability::*;
pub use delegation::*;
pub use flags::*;
pub use gates::*;
pub use keys::*;
pub use revocation::*;
pub use sec_ctx::*;
