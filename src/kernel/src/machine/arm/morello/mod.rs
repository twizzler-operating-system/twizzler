#[cfg(machine = "morello")]
mod qemu;

#[cfg(machine = "morello")]
pub use qemu::*;

#[cfg(machine = "bhyve")]
mod bhyve;

#[cfg(machine = "bhyve")]
pub use bhyve::*;
