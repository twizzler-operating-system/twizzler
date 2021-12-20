#![cfg_attr(not(std), no_std)]
#![feature(asm)]
#![feature(naked_functions)]

mod arch;

#[cfg(feature = "rt")]
mod rt1;
pub mod syscall;

pub fn ready() {}
