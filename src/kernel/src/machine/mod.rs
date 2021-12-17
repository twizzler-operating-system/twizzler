pub mod pc;

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::AtomicU32;
use core::sync::atomic::Ordering;

#[allow(unused_imports)]
pub use pc::*;

use crate::log::KernelConsoleHardware;

pub struct MachineConsoleHardware {
    serial: UnsafeCell<uart_16550::SerialPort>,
    init_state: AtomicU32,
}

impl KernelConsoleHardware for MachineConsoleHardware {
    fn write(&self, data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
        loop {
            let state = self.init_state.load(Ordering::SeqCst);
            if state == 2 {
                break;
            }
            if state == 0
                && self
                    .init_state
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                unsafe {
                    self.serial.get().as_mut().unwrap().init();
                }
                self.init_state.store(2, Ordering::SeqCst);
            }
        }
        unsafe {
            let _ = self
                .serial
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
            serial: unsafe { UnsafeCell::new(uart_16550::SerialPort::new(0x3f8)) },
            init_state: AtomicU32::new(0),
        }
    }
}

/*
#[macro_export]
macro_rules! sprint {
    ($($arg:tt)*) => {
        $crate::machine::serial::_print(format_args!($($arg)*))
    };
}

macro_rules! sprintln {
    () => {
        $crate::sprint!("\n")
    };
    ($fmt:expr) => {
        $crate::sprint!(concat!($fmt, "\n"))
    };
    ($fmt:expr, $($arg:tt)*) => {
        $crate::sprint!(concat!($fmt, "\n"), $($arg)*)
    };
}
*/
