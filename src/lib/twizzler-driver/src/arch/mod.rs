#[cfg(target_arch = "x86_64")]
pub(crate) mod x86;

#[cfg(target_arch = "x86_64")]
pub use x86::*;

#[cfg(target_arch = "aarch64")]
pub(crate) mod aarch64;

#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
