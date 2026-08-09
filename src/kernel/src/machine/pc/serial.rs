use core::{
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering},
};

use crate::{
    interrupt::{Destination, TriggerMode},
    once::Once,
    panic::is_panicing,
};

pub struct SerialPort {
    port: u16,
}

/// Cycles to wait for the uart to accept a byte before dropping it.
///
/// A byte at 115200 baud is ~87us, so this is a ~500x margin at any plausible tsc rate and cannot
/// trip on a working port. It exists because the unbounded version of this wait is a silent-hang
/// generator: `_print_normal` calls it with interrupts disabled while holding the console lock, so
/// a uart that stops asserting OUTPUT_EMPTY -- which host-side backpressure on qemu's chardev can
/// cause under a loaded sweep -- pins that cpu at 100%, blocks every other cpu that tries to print,
/// and produces exactly the signature this document calls a boot hang: no panic, no further output,
/// a transcript ending on a complete line, and nothing for the guest-state dump to find.
///
/// Dropping console bytes can corrupt the `REPORT` protocol the harness parses. That is the right
/// trade: it only happens where the alternative is a machine that never comes back.
const TX_TIMEOUT_CYCLES: u64 = 100_000_000;

/// Set when a send times out, so a stuck port degrades to dropped output rather than making every
/// subsequent byte pay the full timeout.
static TX_STUCK: AtomicBool = AtomicBool::new(false);

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
        unsafe {
            x86::io::outb(self.port + reg, val);
        }
    }

    /// Read register.
    /// # Safety
    /// Must be a valid register in the serial port register space.
    pub unsafe fn read_reg(&self, reg: u16) -> u8 {
        unsafe { x86::io::inb(self.port + reg) }
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
        if !self.wait_for_tx() {
            // Drop it. See TX_TIMEOUT_CYCLES: the alternative is not "output arrives late", it is
            // the whole machine wedging silently.
            return;
        }
        unsafe {
            self.write_reg(Self::DATA, byte);
        }
    }

    /// Wait for the transmit holding register to drain, giving up rather than spinning forever.
    ///
    /// Returns false if the byte should be dropped.
    fn wait_for_tx(&mut self) -> bool {
        // Already known stuck: look once, and do not pay the timeout again per byte. A port that
        // comes back clears the flag here, so this recovers on its own.
        if TX_STUCK.load(Ordering::Relaxed) {
            if self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY) {
                TX_STUCK.store(false, Ordering::Relaxed);
                return true;
            }
            return false;
        }
        let start = unsafe { x86::time::rdtsc() };
        while !self.line_sts().contains(LineStsFlags::OUTPUT_EMPTY) {
            if unsafe { x86::time::rdtsc() }.wrapping_sub(start) > TX_TIMEOUT_CYCLES {
                TX_STUCK.store(true, Ordering::Relaxed);
                return false;
            }
            core::hint::spin_loop();
        }
        true
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
        if is_panicing() {
            return SimpleGuard { lock: self, int };
        }
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

#[must_use = "a dropped guard releases immediately; bind it to a variable"]
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

static SERIAL1: Once<SimpleLock<SerialPort>> = Once::new();

fn serial1() -> &'static SimpleLock<SerialPort> {
    SERIAL1.call_once(|| {
        let mut serial_port = unsafe { SerialPort::new(0x3f8) };
        serial_port.init();
        SimpleLock::new(serial_port)
    })
}

static SERIAL2: Once<SimpleLock<SerialPort>> = Once::new();

fn serial2() -> &'static SimpleLock<SerialPort> {
    SERIAL2.call_once(|| {
        let mut serial_port = unsafe { SerialPort::new(0x2f8) };
        serial_port.init();
        SimpleLock::new(serial_port)
    })
}

pub fn late_init() {
    crate::arch::set_interrupt(
        36,
        false,
        TriggerMode::Edge,
        crate::interrupt::PinPolarity::ActiveHigh,
        Destination::Bsp,
    );
    crate::arch::set_interrupt(
        35,
        false,
        TriggerMode::Edge,
        crate::interrupt::PinPolarity::ActiveHigh,
        Destination::Bsp,
    );
    interrupt_handler();
}

fn do_interrupt(serial: &mut SerialPort, mut buf: &mut [u8]) -> usize {
    let status = serial.read_iid();
    let mut count = 0;
    match (status >> 1) & 7 {
        0 => {
            let _msr = serial.read_modem_status();
            0
        }
        _ => loop {
            let x = serial.receive();
            buf[0] = x;
            buf = &mut buf[1..];
            count += 1;
            if !serial.line_sts().contains(LineStsFlags::INPUT_FULL) || buf.len() == 0 {
                break count;
            }
        },
    }
}

pub fn interrupt_handler() {
    let mut serial = serial1().lock();
    let mut buf = [0; 128];
    let count = do_interrupt(&mut *serial, &mut buf);
    drop(serial);
    for b in &buf[0..count] {
        crate::log::push_input_byte(*b, false);
    }

    let mut serial = serial2().lock();
    let mut buf = [0; 128];
    let count = do_interrupt(&mut *serial, &mut buf);
    drop(serial);
    for b in &buf[0..count] {
        crate::log::push_input_byte(*b, true);
    }
}

pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags, debug: bool) {
    unsafe {
        if debug {
            let _ = serial2()
                .lock()
                .write_str(core::str::from_utf8_unchecked(data));
        } else {
            let _ = serial1()
                .lock()
                .write_str(core::str::from_utf8_unchecked(data));
        }
    }
}
