use lazy_static::lazy_static;
use twizzler_abi::{device::CacheType, object::Protections};

use super::{
    super::uart::PL011,
    gpio::{GpioPin, FunctionOperation, PullSelect, PIN_MANAGER},
};
use super::memory::mmio::PL011_UART;

use crate::memory::{VirtAddr, pagetables::{
    ContiguousProvider, MappingCursor, MappingSettings, Mapper,
    MappingFlags,
}};

lazy_static! {
    // TODO: add a spinlock here
    pub static ref SERIAL: PL011 = {
        // the desired virtal address for the UART's MMIO
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

        // Device memory only prevents speculative data accesses, so we must not
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
        // TODO: fix uart config
        // maybe we need some barriers after writing to mmio
        // serial_port.early_init();

        // Now that the UART object is ready, we must configure how we will
        // output the data from the device. We reserve GPIO pins 14/15 (TX/RX)
        // for this purpose.
        PIN_MANAGER.set_function(GpioPin::Pin14, FunctionOperation::AlternateFunction0);
        PIN_MANAGER.set_function(GpioPin::Pin15, FunctionOperation::AlternateFunction0);
        PIN_MANAGER.set_pull(GpioPin::Pin14, PullSelect::NoResistor);
        PIN_MANAGER.set_pull(GpioPin::Pin15, PullSelect::NoResistor);

        serial_port
    };
}

impl PL011 {
    pub fn write_str(&self, s: &str) {
        for byte in s.bytes() {
            self.tx_byte(byte);
        }
    }

    /// initalize the UART driver early, before interrupts in the system are enabled
    pub fn early_init(&self) {
        // TODO: get clock rate (set statically in config.txt)
        // let's get the actual value from the firmware!!!
        // clock default = 48000000
        const CLOCK: u32 = 48000000; 
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
