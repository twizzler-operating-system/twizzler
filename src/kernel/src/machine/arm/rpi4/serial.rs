use lazy_static::lazy_static;
use twizzler_abi::{device::CacheType, object::Protections};

use super::super::uart::PL011;        
use super::memory::mmio::PL011_UART;

use crate::memory::{VirtAddr, pagetables::{
    ContiguousProvider, MappingCursor, MappingSettings, Mapper,
    MappingFlags,
}};

lazy_static! {
    // TODO: add a spinlock here
    pub static ref SERIAL: PL011 = {
        // the desired virtal address for this region of mmio
        let uart_mmio_base = VirtAddr::new(0xFFFF_0000_0000_0000).unwrap();
        // configure mapping settings for this region of memory
        let cursor = MappingCursor::new(
            uart_mmio_base,
            PL011_UART.length,
        );
        let mut phys = ContiguousProvider::new(
            PL011_UART.start,
            PL011_UART.length,
        );
        // Device memory only prevetns speculative data accesses, so we must not
        // make this region executable to prevent speculative instruction accesses.
        let settings = MappingSettings::new(
            Protections::READ | Protections::WRITE,
            CacheType::MemoryMappedIO,
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
        serial_port.early_init();
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
    fn early_init(&self) {
        const CLOCK: u32 = 0x16e3600; // 24 MHz, TODO: get clock rate
        const BAUD: u32 = 115200;
        // configure the UART with the desired baud, given the clock rate
        unsafe { 
            self.init(CLOCK, BAUD); 
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
