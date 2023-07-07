/// More information about the GPIO Pins can be found in Chapter 5
/// in the "BCM2711 ARM Peripherals" document here:
///     https://datasheets.raspberrypi.com/bcm2711/bcm2711-peripherals.pdf

// Steps to enable UART on GPIO pins
// 1. configure the GPFSEL functions for pins 14/15
// 1.a? configure GPIO pull on pins?
// 2. configure uart at a particular region

// * GPIO base register address at 0x7e200000 (5.2)
//   - All memory accesses to GPIO registers is assumed to be 32-bits
//   - We want to modify the settings of GPFSEL1 since it can configure GPIO 14/15
//   - GPFSEL1 (0x04): FSEL14/FSEL15 are what we want, set to 100 (alt func 0)
//   - not sure if we need to mess with GPSET0 (probs not)
//   - probably have to mess with GPIO_PUP_PDN_CNTRL_REG0 Register (00 no pull)
// * access to peripherals require some constraints on memory ordering (1.3)
//   - AMBA AXI system on BCM2711
//   - switching to different peripherals may make date arrive out of order
//   - memory barriers before first write, and after last read of barrier

use lazy_static::lazy_static;
use registers::{
    register_bitfields, register_structs,
    registers::ReadWrite,
    interfaces::ReadWriteable,
};
use twizzler_abi::{device::CacheType, object::Protections};

use super::memory::mmio::GPIO_PINS;

use crate::memory::{VirtAddr, pagetables::{
    ContiguousProvider, MappingCursor, MappingSettings, Mapper,
    MappingFlags,
}};

// Intance of a pin manager
lazy_static! {
    pub static ref PIN_MANAGER: PinManager = {
        // the desired virtal address for the GPIO Pins' MMIO registers
        let gpio_mmio_base = VirtAddr::new(0xFFFF_0000_0000_6000).unwrap();
        // configure mapping settings for this region of memory
        let cursor = MappingCursor::new(
            gpio_mmio_base,
            GPIO_PINS.length,
        );
        let mut phys = ContiguousProvider::new(
            GPIO_PINS.start,
            GPIO_PINS.length,
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
        PinManager::new(gpio_mmio_base.into())
    };
}

register_structs! {
    #[allow(non_snake_case)]
    /// Layout of GPIO registers in MMIO according to Table 5.2
    /// Some register definitions are ommited, and marked with padding
    GpioRegisters {
        (0x00 => _reserved0),
        // Manages functions for GPIO Pins 10-19
        (0x04 => GPFSEL1: ReadWrite<u32, GPFSEL1::Register>),
        (0x08 => _reserved1),
        // Controls the GPIO Pull-up/Pull-down resistors for pins 0-15
        (0xE4 => GPIO_PUP_PDN_CNTRL_REG0: ReadWrite<u32, GPIO_PUP_PDN_CNTRL_REG0::Register>),
        (0xE8 => _reserved2),
        (0xF4 => @END),
    }
}

// Definition of register bitfields for GPIO registers
register_bitfields! [
    // size of all accesses are 32-bits
    u32,

    /// GPIO Function Select 1 (Pins 10-19)
    GPFSEL1 [
        /// Function select for pin 15
        FSEL15 OFFSET(15) NUMBITS(3) [
            AlternateFunction0 = 0b100
        ],
        /// Function select for pin 14
        FSEL14 OFFSET(12) NUMBITS(3) [
            AlternateFunction0 = 0b100
        ]
    ],

    /// GPIO Pull-up/Pull-down control (Pins 0-15)
    GPIO_PUP_PDN_CNTRL_REG0 [
        /// Resistor select for GPIO 15
        GPIO_PUP_PDN_CNTRL15 OFFSET(30) NUMBITS(2) [
            NoResistor = 0b00
        ],
        /// Resistor select for GPIO 14
        GPIO_PUP_PDN_CNTRL14 OFFSET(28) NUMBITS(2) [
            NoResistor = 0b00
        ]
    ],
];

#[repr(u8)]
pub enum FunctionOperation {
    Input = 0b000,
    Output = 0b001,
    AlternateFunction0 = 0b100,
    AlternateFunction1 = 0b101,
}

#[repr(u32)]
pub enum GpioPin {
    Pin14 = 14,
    Pin15 = 15,
}

#[repr(u8)]
pub enum PullSelect {
    NoResistor,
    PullUp,
    PullDown,
    Reserved
}

struct Pins {
    base: usize // base mmio address
}

pub struct PinManager {
    pins: Pins,
}

impl PinManager {
    pub const fn new(addr: usize) -> Self {
        Self {
            pins: Pins {
                base: addr,
            }
        }
    }

    // Configures the function that a GPIO pin serves, see [FunctionOperation]
    pub fn set_function(&self, gpio: GpioPin, _alt: FunctionOperation) {
        match gpio {
            // TODO: convert pins to some index here ...?
            // TODO: use the set part to write the function value???
            GpioPin::Pin14 => self.pins.GPFSEL1.modify(GPFSEL1::FSEL14::AlternateFunction0),
            GpioPin::Pin15 => self.pins.GPFSEL1.modify(GPFSEL1::FSEL15::AlternateFunction0),
        }
    }

    // Configures the pull of a particular GPIO pin 
    pub fn set_pull(&self, gpio: GpioPin, _pull: PullSelect) {
        match gpio {
            GpioPin::Pin14 => self.pins.GPIO_PUP_PDN_CNTRL_REG0.modify(GPIO_PUP_PDN_CNTRL_REG0::GPIO_PUP_PDN_CNTRL14::NoResistor),
            GpioPin::Pin15 => self.pins.GPIO_PUP_PDN_CNTRL_REG0.modify(GPIO_PUP_PDN_CNTRL_REG0::GPIO_PUP_PDN_CNTRL15::NoResistor),
        }
    }
}

use core::ops::Deref;
impl Deref for Pins {
    type Target = GpioRegisters;

    fn deref(&self) -> &Self::Target {
        unsafe { &*(self.base as *const GpioRegisters) }
    }
}
