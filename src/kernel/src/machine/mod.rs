pub(crate) mod pcie;
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

    #[cfg(target_arch = "x86_64")]
    fn queue(&self, data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
        serial::queue(data, self.debug);
    }

    #[cfg(target_arch = "x86_64")]
    fn pump(&self) -> bool {
        serial::pump(self.debug)
    }

    #[cfg(target_arch = "x86_64")]
    fn pending(&self) -> bool {
        serial::pending(self.debug)
    }
}

impl MachineConsoleHardware {
    pub const fn new(debug: bool) -> Self {
        Self { debug }
    }
}
