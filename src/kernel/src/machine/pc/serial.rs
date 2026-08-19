use core::{
    cell::UnsafeCell,
    fmt::Write,
    sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
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

/// Bytes that may be written after a single OUTPUT_EMPTY check.
///
/// On a 16550A, OUTPUT_EMPTY means the whole 16-byte transmit FIFO is empty, not just the holding
/// register, so the poll is only needed once per FIFO-full rather than once per byte. Both the poll
/// and the write are port accesses, which under virtualization are vm exits, so this nearly halves
/// the exits per byte of console output -- and console output is not cheap here: a boot spends
/// close to a second of wall clock inside `KernelConsoleWrite`.
///
/// Stays at 1 unless [`SerialPort::init`] confirms the part actually enabled its FIFO.
static TX_BURST: AtomicU32 = AtomicU32::new(1);

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

            // IID bits 7:6 both set is the 16550A's report that the FIFO it was just asked for is
            // on. Anything else (an 8250, or a 16550 with the broken FIFO) keeps the burst at one.
            if self.read_reg(Self::IID) & 0xc0 == 0xc0 {
                TX_BURST.store(16, Ordering::Relaxed);
            }
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
        for chunk in s
            .as_bytes()
            .chunks(TX_BURST.load(Ordering::Relaxed) as usize)
        {
            if !self.wait_for_tx() {
                // Stuck port: dropping the rest is the same trade `send` makes.
                return Ok(());
            }
            for byte in chunk {
                unsafe { self.write_reg(Self::DATA, *byte) };
            }
        }
        Ok(())
    }
}

struct SimpleLock<T> {
    data: UnsafeCell<T>,
    state: AtomicBool,
}

/// Whether this cpu is already inside a [`SimpleLock`] spin; see the drain in `lock`.
#[thread_local]
static IN_SPIN: core::cell::Cell<bool> = core::cell::Cell::new(false);

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
        // Interrupts are off for the whole spin, so without draining shootdowns here this cpu is
        // deaf to them for as long as it waits -- and every console write in the kernel, emergency
        // ones included, comes through this lock. A sender in `PendingShootdown::do_wait` then
        // spins until we happen to finish; the sweeps have that on record twice ("TLB shootdown
        // stalled on CPUs 0 -> 1 (4194304 iterations)").
        //
        // Reentrancy is why this is not simply `spin_wait_until`: the drain can reach
        // `TlbShootdownInfo::complete`, whose stuck-lock report is an `emerglogln!` -- back into
        // this same lock, which on a ticket lock would be a second ticket and on this one a spin
        // against ourselves. The flag makes the nested attempt spin plainly, exactly as it did
        // before; only the outermost waiter drains.
        // Gated on per-cpu state existing at all. This lock is taken by the very first console
        // writes in boot -- before TLS, before the APIC -- and both the flag below and the drain
        // (`current_processor()`, by way of the shootdown handler) need it. Draining early panics
        // in the interrupt path with "got interrupt before initializing APIC", which is a boot
        // failure rather than the deadlock it was meant to avoid.
        let drain = crate::processor::tls_ready();
        let nested = drain && IN_SPIN.replace(true);
        let mut iters = 0u32;
        while self
            .state
            .compare_exchange_weak(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            iters = iters.wrapping_add(1);
            if drain && !nested && iters % 100 == 0 {
                crate::arch::processor::spin_wait_iteration();
            }
            core::hint::spin_loop()
        }
        if drain {
            IN_SPIN.set(nested);
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

/// Returns the bytes drained and the IIR value that decided the arm, so the caller can report what
/// the handler saw without re-reading any uart register.
fn do_interrupt(serial: &mut SerialPort, mut buf: &mut [u8]) -> (usize, u8) {
    let status = serial.read_iid();
    let mut count = 0;
    let drained = match (status >> 1) & 7 {
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
    };
    (drained, status)
}

/// DIAG (B1): entries into the serial ISR.
///
/// The shutdown hang's guest dump puts a cpu at CPL=0 with RIP inside `interrupt_handler` in 4 of
/// the 5 instances captured in `many-b1probe`, so count entries rather than infer from a register
/// file. Normal operation takes one entry per console-input event and never comes near the
/// threshold: **a report at all means the handler is re-firing without its condition clearing**,
/// and the `iid` says which arm is failing to clear it.
///
/// Two properties this deliberately has. It reads no uart register of its own -- `iid` is the value
/// `do_interrupt` already read -- because reading LSR or MSR here would clear interrupt conditions
/// and could mask the livelock it exists to catch. And it reports with `emerglogln`, which takes no
/// console lock (see `log::_print_emergency`), so it is safe from interrupt context even when the
/// interrupted thread holds one -- which is the likely case here, since the hang follows a
/// `force_exit` backtrace.
static ISR_ENTRIES: AtomicU64 = AtomicU64::new(0);
static ISR_REPORTS: AtomicU32 = AtomicU32::new(0);
const ISR_REPORT_EVERY: u64 = 1 << 16;
const ISR_REPORT_BUDGET: u32 = 8;

fn report_isr_rate(iid1: u8, count1: usize, iid2: u8, count2: usize) {
    let entries = ISR_ENTRIES.fetch_add(1, Ordering::Relaxed) + 1;
    if entries % ISR_REPORT_EVERY != 0 {
        return;
    }
    if ISR_REPORTS.fetch_add(1, Ordering::Relaxed) >= ISR_REPORT_BUDGET {
        return;
    }
    emerglogln!(
        "serial isr: {} entries -- com1 iid {:#04x} (id {}) drained {}, com2 iid {:#04x} (id {}) drained {}",
        entries,
        iid1,
        (iid1 >> 1) & 7,
        count1,
        iid2,
        (iid2 >> 1) & 7,
        count2,
    );
}

pub fn interrupt_handler() {
    let mut serial = serial1().lock();
    let mut buf = [0; 128];
    let (count1, iid1) = do_interrupt(&mut *serial, &mut buf);
    drop(serial);
    for b in &buf[0..count1] {
        crate::log::push_input_byte(*b, false);
    }

    let mut serial = serial2().lock();
    let mut buf = [0; 128];
    let (count2, iid2) = do_interrupt(&mut *serial, &mut buf);
    drop(serial);
    for b in &buf[0..count2] {
        crate::log::push_input_byte(*b, true);
    }

    report_isr_rate(iid1, count1, iid2, count2);
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
