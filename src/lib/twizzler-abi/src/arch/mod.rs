//! Architecture-dependent code, will include submodules for the appropriate arch that expose the
//! _start symbol and the raw_syscall symbol.

mod x86_64;

pub use x86_64::*;
