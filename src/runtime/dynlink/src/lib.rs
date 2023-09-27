#![feature(strict_provenance)]
#![feature(never_type)]
#![feature(iterator_try_collect)]
#![feature(result_flattening)]

pub mod addr;
pub mod compartment;
pub mod context;
pub mod library;
pub mod symbol;

#[cfg(feature = "std")]
use std::alloc;

use library::LibraryId;

#[cfg(not(feature = "std"))]
extern crate alloc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LookupError {
    NotFound,
    Unloaded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AddLibraryError {
    NotFound,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdvanceError {
    LibraryFailed(LibraryId),
    EndState,
}
