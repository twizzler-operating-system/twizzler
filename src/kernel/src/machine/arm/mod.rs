/// QEMU virt target for aarch64
#[cfg(machine = "virt")]
mod virt;

/// Raspberry Pi 4 (bcm2711)
#[cfg(machine = "rpi4")]
mod rpi4;

#[cfg(machine = "virt")]
pub use virt::*;

#[cfg(machine = "rpi4")]
pub use rpi4::*;

mod common;
