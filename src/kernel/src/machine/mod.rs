pub mod pc;

use core::fmt::Write;

#[allow(unused_imports)]
pub use pc::*;

use crate::log::KernelConsoleHardware;

pub struct MachineConsoleHardware;

impl KernelConsoleHardware for MachineConsoleHardware {
    fn write(&self, data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
        unsafe {
            let _ = serial::SERIAL1
                .lock()
                .write_str(core::str::from_utf8_unchecked(data));
        }
    }
}

impl MachineConsoleHardware {
    pub const fn new() -> Self {
        Self
    }
}
