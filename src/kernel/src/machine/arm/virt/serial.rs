use lazy_static::lazy_static;
use twizzler_abi::object::Protections;

use super::super::common::uart::PL011;        

use crate::memory::{PhysAddr, pagetables::{
    ContiguousProvider, MappingCursor, MappingSettings, Mapper,
    MappingFlags,
}};
use crate::arch::memory::mmio::MMIO_ALLOCATOR;

lazy_static! {
    // TODO: add a spinlock here
    pub static ref SERIAL: PL011 = {
        let (clock_freq, mmio) = crate::machine::info::get_uart_info();
        // the desired virtal address for this region of mmio
        let uart_mmio_base = {
            MMIO_ALLOCATOR.lock().alloc(mmio.length as usize)
                .expect("failed to allocate MMIO region")
        };
        // configure mapping settings for this region of memory
        let cursor = MappingCursor::new(
            uart_mmio_base,
            mmio.length as usize,
        );
        let mut phys = ContiguousProvider::new(
            unsafe { PhysAddr::new_unchecked(mmio.info) },
            mmio.length as usize,
        );
        // Device memory only prevetns speculative data accesses, so we must not
        // make this region executable to prevent speculative instruction accesses.
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            mmio.cache_type,
            MappingFlags::GLOBAL,
        );
        // map in with curent memory context
        unsafe {
            let mut mapper = Mapper::current();
            mapper.map(cursor, &mut phys, &settings);
        }

        // create instance of the PL011 UART driver
        let serial_port = unsafe { 
            PL011::new(uart_mmio_base.into()) 
        };
        serial_port.early_init(clock_freq as u32);
        serial_port
    };
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
}

pub fn write(data: &[u8], _flags: crate::log::KernelConsoleWriteFlags) {
    // We need the memory management system up and running to use MMIO.
    // Other requests to log to the console are ignored. The console is
    // initialized lazily on first access.
    //
    // This means that we cannot and should not ouput logging messages to
    // the UART before this happens. Mapping in some memory might require
    // allocating physical frames for the page tables.
    if crate::memory::is_init() {
        unsafe {
            SERIAL.write_str(core::str::from_utf8_unchecked(data));
        }
    }
}
