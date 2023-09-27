#![feature(strict_provenance)]
#![feature(never_type)]
#![feature(iterator_try_collect)]
#![feature(result_flattening)]

pub mod addr;
pub mod compartment;
pub mod context;
pub mod error;
pub mod library;
pub mod symbol;

#[cfg(feature = "std")]
use std::alloc;

#[cfg(not(feature = "std"))]
extern crate alloc;

pub use error::*;
