use std::io::{BufRead, BufReader, ErrorKind, Read};

use twizzler_abi::syscall::{
    sys_kernel_console_read, sys_kernel_console_write, KernelConsoleReadFlags,
    KernelConsoleWriteFlags,
};

struct TwzIo;

impl Read for TwzIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        sys_kernel_console_read(buf, KernelConsoleReadFlags::empty())
            .map_err(|_| ErrorKind::Other.into())
    }
}

fn main() {
    sys_kernel_console_write(b"SEQUENCE START\n", KernelConsoleWriteFlags::empty());

    let reader = BufReader::new(TwzIo);
    for line in reader.lines() {
        if let Some(line) = line.ok() {
            println!("{}", line);
        }
    }
}
