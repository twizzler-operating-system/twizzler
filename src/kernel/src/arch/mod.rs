#[cfg(target_arch = "x86_64")]
mod amd64;

#[cfg(target_arch = "x86_64")]
pub use amd64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
