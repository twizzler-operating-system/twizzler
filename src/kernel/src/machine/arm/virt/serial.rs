use twizzler_abi::object::Protections;

use super::super::common::uart::PL011;
use crate::{
    arch::memory::mmio::mmio_allocator,
    interrupt::{Destination, TriggerMode},
    memory::{
        pagetables::{ContiguousProvider, Mapper, MappingCursor, MappingFlags, MappingSettings},
        PhysAddr,
    },
    once::Once,
};

pub fn serial() -> &'static PL011 {
    SERIAL.call_once(|| {
        let (clock_freq, mmio) = crate::machine::info::get_uart_info();
        // the desired virtal address for this region of mmio
        let uart_mmio_base = {
            mmio_allocator()
                .lock()
                .alloc(mmio.length as usize)
                .expect("failed to allocate MMIO region")
        };
        // configure mapping settings for this region of memory
        let cursor = MappingCursor::new(uart_mmio_base, mmio.length as usize);
        // Device memory only prevetns speculative data accesses, so we must not
        // make this region executable to prevent speculative instruction accesses.
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            mmio.cache_type,
            MappingFlags::GLOBAL,
        );
        let mut phys = ContiguousProvider::new(
            unsafe { PhysAddr::new_unchecked(mmio.info) },
            mmio.length as usize,
            settings,
        );
        // map in with curent memory context
        unsafe {
            let mut mapper = Mapper::current();
            mapper.map(cursor, &mut phys);
        }

        // create instance of the PL011 UART driver
        let serial_port = unsafe { PL011::new(uart_mmio_base.into()) };
        serial_port.early_init(clock_freq as u32);
        serial_port
    })
}
// TODO: add a spinlock here
static SERIAL: Once<PL011> = Once::new();

static SERIAL_INT_ID: Once<u32> = Once::new();

pub fn serial_int_id() -> u32 {
    *SERIAL_INT_ID.call_once(|| {
        let int_num = crate::machine::info::get_uart_interrupt_num()
            .expect("failed to decode UART interrupt number");
        int_num
    })
}

impl PL011 {
    fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.tx_byte(byte);
        }
    }

    /// initalize the UART driver early, before interrupts in the system are enabled
    fn early_init(&self, clock_freq: u32) {
        const BAUD: u32 = 115200;
        // configure the UART with the desired baud, given the clock rate
        unsafe {
            self.init(clock_freq, BAUD);
        }
    }

    /// intitialize the UART driver after the system has enabled interrupts
    pub fn late_init(&self) {
        // enable the rx side to use interrupts
        unsafe {
            self.enable_rx_interrupt();
        }

        crate::arch::set_interrupt(
            serial_int_id(),
            false,
            TriggerMode::Edge,
            crate::interrupt::PinPolarity::ActiveHigh,
            Destination::Bsp,
        );
    }
}

pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags, _debug: bool) {
    // We need the memory management system up and running to use MMIO.
    // Other requests to log to the console are ignored. The console is
    // initialized lazily on first access.
    //
    // This means that we cannot and should not ouput logging messages to
    // the UART before this happens. Mapping in some memory might require
    // allocating physical frames for the page tables.
    if crate::memory::is_init() {
        unsafe {
            serial().write_str(core::str::from_utf8_unchecked(data));
        }
    }
}

pub fn serial_interrupt_handler() {
    let byte = serial().rx_byte();
    if let Some(x) = byte {
        crate::log::push_input_byte(x, false);
    }
    serial().clear_rx_interrupt();
}
