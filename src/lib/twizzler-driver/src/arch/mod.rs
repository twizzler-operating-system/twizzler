#[cfg(target_arch = "x86_64")]
pub(crate) mod x86;

#[cfg(target_arch = "x86_64")]
pub(crate) use x86::*;
