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

#[derive(Debug)]
pub enum LookupError {
    NotFound,
    Unloaded,
    ParseError(elf::ParseError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AddLibraryError {
    NotFound,
    AdvanceError(AdvanceError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AdvanceError {
    LibraryFailed(LibraryId),
    EndState,
}
