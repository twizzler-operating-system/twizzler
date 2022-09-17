#[cfg(target_arch = "x86_64")]
mod amd64;

#[cfg(target_arch = "x86_64")]
pub use amd64::*;
