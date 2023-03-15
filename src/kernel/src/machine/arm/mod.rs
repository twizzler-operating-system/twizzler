/// QEMU virt target for aarch64
#[cfg(machine = "virt")]
mod virt;

#[cfg(machine = "virt")]
pub use virt::*;

mod uart;
