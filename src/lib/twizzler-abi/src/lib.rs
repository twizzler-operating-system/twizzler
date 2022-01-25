#![cfg_attr(not(feature = "std"), no_std)]
#![feature(asm)]
#![feature(naked_functions)]

mod arch;

pub mod alloc;
pub mod aux;
pub mod object;
#[cfg(feature = "rt")]
mod rt1;
pub mod syscall;

pub fn ready() {}
