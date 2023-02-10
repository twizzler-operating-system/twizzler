use lazy_static::lazy_static;

use super::uart::PL011;

lazy_static! {
    // TODO: add a spinlock here
    pub static ref SERIAL: super::uart::PL011 = {
        const SERIAL_PORT_BASE_ADDRESS: usize = 0x0900_0000; // specific to QEMU
        let serial_port = unsafe { super::uart::PL011::new(SERIAL_PORT_BASE_ADDRESS) };
        const CLOCK: u32 = 0x16e3600; // 24 MHz, TODO: get clock rate
        const BAUD: u32 = 115200;
        unsafe { serial_port.init(CLOCK, BAUD); }
        serial_port
    };
}

impl PL011 {
    fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.tx_byte(byte);
        }
    }
}

pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
    unsafe {
        SERIAL.write_str(core::str::from_utf8_unchecked(data));
    }
}