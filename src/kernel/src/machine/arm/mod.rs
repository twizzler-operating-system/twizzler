/// QEMU virt target for aarch64
#[cfg(machine = "virt")]
mod virt;

#[cfg(machine = "virt")]
pub use virt::*;

/// Morello SDP for CHERI
#[cfg(any(machine = "morello", machine = "bhyve"))]
mod morello;

#[cfg(any(machine = "morello", machine = "bhyve"))]
pub use morello::*;

mod common;
