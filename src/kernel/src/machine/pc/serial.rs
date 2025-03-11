use core::{
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
};

use lazy_static::lazy_static;

use crate::interrupt::{Destination, TriggerMode};

pub struct SerialPort {
    port: u16,
}

bitflags::bitflags! {
    /// Line status flags
    struct LineStsFlags: u8 {
        const INPUT_FULL = 1;
        // 1 to 4 unknown
        const OUTPUT_EMPTY = 1 << 5;
        // 6 and 7 unknown
    }
}

impl SerialPort {
    const INT_EN: u16 = 1;
    const IID: u16 = 2;
    const DATA: u16 = 0;
    const FIFO_CTRL: u16 = 2;
    const LINE_CTRL: u16 = 3;
    const MODEM_CTRL: u16 = 4;
    const LINE_STS: u16 = 5;
    const MODEM_STS: u16 = 6;
    const SCRATCH: u16 = 7;
    /// Construct a new serial port.
    /// # Safety
    /// The supplied port must be a correct, functioning serial port on the system.
    pub unsafe fn new(port: u16) -> Self {
        Self { port }
    }

    /// Write register.
    /// # Safety
    /// Must be a valid register in the serial port register space.
    pub unsafe fn write_reg(&self, reg: u16, val: u8) {
        x86::io::outb(self.port + reg, val);
    }

    /// Read register.
    /// # Safety
    /// Must be a valid register in the serial port register space.
    pub unsafe fn read_reg(&self, reg: u16) -> u8 {
        x86::io::inb(self.port + reg)
    }

    pub fn init(&mut self) {
        unsafe {
            for i in 0..8 {
                self.read_reg(i);
            }
            // Disable interrupts
            self.write_reg(Self::INT_EN, 0x00);

            // Enable DLAB
            self.write_reg(Self::LINE_CTRL, 0x80);

            // Set maximum speed to 115200 bps by configuring DLL and DLM
            self.write_reg(Self::DATA, 0x01);
            self.write_reg(Self::INT_EN, 0x00);

            // Disable DLAB and set data word length to 8 bits
            self.write_reg(Self::LINE_CTRL, 0x03);

            // Enable FIFO, clear TX/RX queues and
            // set interrupt watermark at 14 bytes
            self.write_reg(Self::FIFO_CTRL, 0xC7);

            // Mark data terminal ready, signal request to send
            // and enable auxilliary output #2 (used as interrupt line for CPU)
            self.write_reg(Self::MODEM_CTRL, 0x0F);

            // Enable interrupts
            self.write_reg(Self::INT_EN, 0x01);
            for i in 0..8 {
                self.read_reg(i);
            }
            self.write_reg(Self::MODEM_CTRL, 0x0F);
        }
    }

    fn line_sts(&mut self) -> LineStsFlags {
        unsafe { LineStsFlags::from_bits_truncate(self.read_reg(Self::LINE_STS)) }
    }

    pub fn send(&mut self, byte: u8) {
        unsafe {
            while !self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY) {
                core::hint::spin_loop();
            }
            self.write_reg(Self::DATA, byte);
        }
    }

    pub fn receive(&mut self) -> u8 {
        unsafe { self.read_reg(Self::DATA) }
    }

    pub fn has_pending(&mut self) -> bool {
        let iid = unsafe { self.read_reg(Self::IID) };
        iid & 1 != 0
    }

    pub fn read_modem_status(&mut self) -> u8 {
        unsafe { self.read_reg(Self::MODEM_CTRL) }
    }

    pub fn read_iid(&mut self) -> u8 {
        unsafe { self.read_reg(Self::IID) }
    }
}

impl core::fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            self.send(byte);
            if byte == b'\n' {
                self.send(b'\r');
            }
        }
        Ok(())
    }
}

struct SimpleLock<T> {
    data: UnsafeCell<T>,
    state: AtomicBool,
}

impl<T> SimpleLock<T> {
    fn new(item: T) -> Self {
        Self {
            state: AtomicBool::new(false),
            data: UnsafeCell::new(item),
        }
    }
    fn lock(&self) -> SimpleGuard<'_, T> {
        let int = crate::interrupt::disable();
        while self
            .state
            .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            core::hint::spin_loop()
        }
        SimpleGuard { lock: self, int }
    }
}

struct SimpleGuard<'a, T> {
    lock: &'a SimpleLock<T>,
    int: bool,
}

impl<'a, T> Drop for SimpleGuard<'a, T> {
    fn drop(&mut self) {
        self.lock.state.store(false, Ordering::SeqCst);
        crate::interrupt::set(self.int);
    }
}

impl<T> core::ops::Deref for SimpleGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> core::ops::DerefMut for SimpleGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.lock.data.get() }
    }
}

unsafe impl<T> Send for SimpleLock<T> where T: Send {}
unsafe impl<T> Sync for SimpleLock<T> where T: Send {}
unsafe impl<T> Send for SimpleGuard<'_, T> where T: Send {}
unsafe impl<T> Sync for SimpleGuard<'_, T> where T: Send + Sync {}

lazy_static! {
    static ref SERIAL1: SimpleLock<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x3f8) };
        serial_port.init();
        SimpleLock::new(serial_port)
    };
    static ref SERIAL2: SimpleLock<SerialPort> = {
        let mut serial_port = unsafe { SerialPort::new(0x2f8) };
        serial_port.init();
        SimpleLock::new(serial_port)
    };
}

pub fn late_init() {
    crate::arch::set_interrupt(
        36,
        false,
        TriggerMode::Edge,
        crate::interrupt::PinPolarity::ActiveHigh,
        Destination::Bsp,
    );
}

pub fn interrupt_handler() {
    let mut serial = SERIAL1.lock();
    let status = serial.read_iid();
    match (status >> 1) & 7 {
        0 => {
            let _msr = serial.read_modem_status();
        }
        _ => loop {
            let x = serial.receive();
            drop(serial);
            crate::log::push_input_byte(x);
            serial = SERIAL1.lock();
            if !serial.line_sts().contains(LineStsFlags::INPUT_FULL) {
                break;
            }
        },
    }
}

pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
    unsafe {
        let _ = SERIAL1
            .lock()
            .write_str(core::str::from_utf8_unchecked(data));
        let _ = SERIAL2
            .lock()
            .write_str(core::str::from_utf8_unchecked(data));
    }
}
