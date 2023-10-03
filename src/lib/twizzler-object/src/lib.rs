//! The twizzler-object crate.
//!
//! The purpose of this crate is to provide:
//!
//!  - Object creation and access through handles.
//!  - Access to the base of an object.
//!  - Whole-object lifetime control and deletion.
//!  - Basic types for invariant pointers and metadata.
//!
//! This crate is also designed to create a base to implement higher-level object access crates
//! (such as twizzler-nando), and thus provides unsafe functions to access possibly shared object
//! memory. Additionally, this crate provides a low-level management of mapping objects and tracking
//! memory slots.
//!
//! # Isolation Safety
//!
//! In general, this crate cannot prove isolation safety (that is, single-writer XOR
//! multiple-readers). The twizzler-nando crate can, however, so we defer to that crate for any
//! operations on objects that access memory that could mutate. This limits this crate to only
//! safely providing access to certain parts of objects, such as the base (which we expect to mutate
//! only via interior mutability), and immutable parts of the object metadata. Access to other
//! things in the object is provided via unsafe functions, like raw FOT entries or the meta info
//! struct as a whole.
//!
//! Thus we expect programmers to use the twizzler-nando crate to operate on object data most of the
//! time. The unsafe functions in this crate are provided mostly for the implementation of the
//! twizzler-nando crate.

#![feature(auto_traits)]
//#![feature(specialization)]
#![feature(negative_impls)]

pub use twizzler_abi::object::ObjID;

mod base;
mod create;
mod init;
pub mod marker;
pub mod meta;
mod object;
pub mod ptr;
pub mod slot;

pub use create::*;
pub use init::*;
pub use object::*;
