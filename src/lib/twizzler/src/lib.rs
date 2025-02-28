#![feature(negative_impls)]
#![feature(auto_traits)]
#![feature(strict_provenance)]
#![feature(allocator_api)]
#![allow(internal_features)]
#![feature(core_intrinsics)]

pub mod alloc;
pub mod collections;
pub mod marker;
pub mod object;
pub mod ptr;
pub mod tx;

mod pager;
