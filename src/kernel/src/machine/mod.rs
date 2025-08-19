mod time;

#[cfg(target_arch = "aarch64")]
mod arm;

#[cfg(target_arch = "aarch64")]
pub use arm::*;

#[cfg(target_arch = "x86_64")]
pub mod pc;

#[cfg(target_arch = "x86_64")]
#[allow(unused_imports)]
pub use pc::*;
pub use time::*;

use crate::log::KernelConsoleHardware;

pub struct MachineConsoleHardware {
    debug: bool,
}

impl KernelConsoleHardware for MachineConsoleHardware {
    fn write(&self, data: &[u8], flags: crate::log::KernelConsoleWriteFlags) {
        serial::write(data, flags, self.debug);
    }
}

impl MachineConsoleHardware {
    pub const fn new(debug: bool) -> Self {
        Self { debug }
    }
}
