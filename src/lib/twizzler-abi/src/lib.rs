#![no_std]
#![feature(asm)]
#![feature(naked_functions)]

mod arch;
mod rt1;
pub mod syscall;

pub fn ready() {}
