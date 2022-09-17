mod time;

#[cfg(target_arch = "x86_64")]
pub mod pc;

#[cfg(target_arch = "x86_64")]
#[allow(unused_imports)]
pub use pc::*;
pub use time::*;

use crate::log::KernelConsoleHardware;

pub struct MachineConsoleHardware;

impl KernelConsoleHardware for MachineConsoleHardware {
    fn write(&self, data: &[u8], flags: crate::log::KernelConsoleWriteFlags) {
        serial::write(data, flags);
    }
}

impl MachineConsoleHardware {
    pub const fn new() -> Self {
        Self
    }
}
