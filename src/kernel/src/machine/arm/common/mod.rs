pub mod boot;
pub mod gicv2;
#[cfg(any(machine = "morello", machine = "bhyve"))]
pub mod gicv3;
pub mod mmio;
pub mod uart;
