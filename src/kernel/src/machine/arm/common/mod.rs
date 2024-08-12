pub mod boot;
pub mod gicv2;
#[cfg(machine = "morello")]
pub mod gicv3;
pub mod mmio;
pub mod uart;
