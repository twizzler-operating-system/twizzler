pub static mut UART: *mut u8 = 0x0900_0000 as *mut u8;

pub fn print_char(c: u8) {
    unsafe { core::ptr::write_volatile(UART, c); }
}

pub fn print_str(s: &[u8]) {
    for c in s.iter() {
        print_char(*c);
    }
}

// pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
//     print_str(data)
// }
