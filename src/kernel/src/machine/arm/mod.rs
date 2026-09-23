/// QEMU virt target for aarch64
#[cfg(machine = "virt")]
mod virt;

#[cfg(machine = "virt")]
pub use virt::*;

/// Morello SDP for CHERI
#[cfg(machine = "morello")]
mod morello;

#[cfg(machine = "morello")]
pub use morello::*;

/// Board clocks beyond the generic timer: the virt machine's RTC.
pub fn machine_clocks() {
    #[cfg(machine = "virt")]
    virt::rtc::register();
}

mod common;
