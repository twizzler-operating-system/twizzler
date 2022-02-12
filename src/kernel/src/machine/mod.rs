pub mod pc;

use core::cell::UnsafeCell;
use core::fmt::Write;

#[allow(unused_imports)]
pub use pc::*;
use twizzler_abi::syscall::KernelConsoleReadError;
use twizzler_abi::syscall::KernelConsoleReadFlags;

use crate::log::KernelConsoleHardware;
use crate::once::Once;

pub struct MachineConsoleHardware {
    serial: Once<UnsafeCell<uart_16550::SerialPort>>,
}

impl MachineConsoleHardware {
    fn init(&self) {
        self.serial.call_once(|| {
            let mut s = unsafe { uart_16550::SerialPort::new(0x3f8) };
            s.init();
            UnsafeCell::new(s)
        });
    }
}

// TODO: have a separate receive thread running to collect data into a buffer.
impl KernelConsoleHardware for MachineConsoleHardware {
    fn read(
        &self,
        data: &mut [u8],
        _flags: KernelConsoleReadFlags,
    ) -> Result<usize, KernelConsoleReadError> {
        self.init();
        let mut c = 0;
        for i in 0..data.len() {
            let v = unsafe { self.serial.wait().get().as_mut().unwrap().receive() };
            match v {
                13 => {
                    log!("\n");
                    data[i] = 10;
                    c += 1;
                    break;
                }
                4 => break,
                _ => {
                    log!("{}", v as char);
                    data[i] = v
                }
            }
            c += 1;
        }
        Ok(c)
    }

    fn write(&self, data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
        self.init();
        unsafe {
            let _res = self
                .serial
                .wait()
                .get()
                .as_mut()
                .unwrap()
                .write_str(core::str::from_utf8_unchecked(data));
        }
    }
}

impl MachineConsoleHardware {
    pub const fn new() -> Self {
        Self {
            serial: Once::new(),
        }
    }
}
