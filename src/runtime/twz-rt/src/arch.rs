#[cfg(target_arch = "x86_64")]
mod x86_64;

#[allow(unused_imports)]
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;

#[allow(unused_imports)]
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
