use crate::{kso::KactionError, vcell::Volatile};
/// The base struct for an info sub-object for a PCIe bus.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug)]
pub struct PcieInfo {
    pub bus_start: u8,
    pub bus_end: u8,
    pub seg_nr: u16,
}

/// The base struct for an info sub-object for a PCIe device.
#[allow(dead_code)]
#[repr(C)]
#[derive(Debug)]
pub struct PcieDeviceInfo {
    pub seg_nr: u16,
    pub bus_nr: u8,
    pub dev_nr: u8,
    pub func_nr: u8,
    pub device_id: u16,
    pub vendor_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub progif: u8,
    pub revision: u8,
}

/// PCIe-specific [crate::kso::KactionGenericCmd] values.
#[repr(u32)]
pub enum PcieKactionSpecific {
    /// Register a device ID.
    RegisterDevice = 0,
    /// Allocate an interrupt for a device.
    AllocateInterrupt = 1,
}

impl From<PcieKactionSpecific> for u32 {
    fn from(x: PcieKactionSpecific) -> Self {
        x as u32
    }
}

impl TryFrom<u32> for PcieKactionSpecific {
    type Error = KactionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => PcieKactionSpecific::RegisterDevice,
            1 => PcieKactionSpecific::AllocateInterrupt,
            _ => return Err(KactionError::InvalidArgument),
        })
    }
}

//TODO: can we move this out of this crate?
/// The standard PCIe function header.
/// See the PCI spec for more details.
#[allow(dead_code)]
#[repr(packed(4096))]
pub struct PcieFunctionHeader {
    pub vendor_id: Volatile<u16>,
    pub device_id: Volatile<u16>,
    pub command: Volatile<u16>,
    pub status: Volatile<u16>,
    pub revision: Volatile<u8>,
    pub progif: Volatile<u8>,
    pub subclass: Volatile<u8>,
    pub class: Volatile<u8>,
    pub cache_line_size: Volatile<u8>,
    pub latency_timer: Volatile<u8>,
    pub header_type: Volatile<u8>,
    pub bist: Volatile<u8>,
}

/// The standard PCIe device header.
/// See the PCI spec for more details.
#[allow(dead_code)]
#[repr(packed)]
pub struct PcieDeviceHeader {
    pub fnheader: PcieFunctionHeader,
    pub bars: [Volatile<u32>; 6],
    pub cardbus_cis_ptr: Volatile<u32>,
    pub subsystem_vendor_id: Volatile<u16>,
    pub subsystem_id: Volatile<u16>,
    pub exprom_base: Volatile<u32>,
    pub cap_ptr: Volatile<u32>,
    res0: Volatile<u32>,
    pub int_line: Volatile<u8>,
    pub int_pin: Volatile<u8>,
    pub min_grant: Volatile<u8>,
    pub max_latency: Volatile<u8>,
}

/// The standard PCIe bridge header.
/// See the PCI spec for more details.
#[allow(dead_code)]
#[repr(packed)]
pub struct PcieBridgeHeader {
    pub fnheader: PcieFunctionHeader,
    pub bar: [Volatile<u32>; 2],
    pub primary_bus_nr: Volatile<u8>,
    pub secondary_bus_nr: Volatile<u8>,
    pub subordinate_bus_nr: Volatile<u8>,
    pub secondary_latency_timer: Volatile<u8>,
    pub io_base: Volatile<u8>,
    pub io_limit: Volatile<u8>,
    pub secondary_status: Volatile<u8>,
    pub memory_base: Volatile<u16>,
    pub memory_limit: Volatile<u16>,
    pub pref_memory_base: Volatile<u16>,
    pub pref_memory_limit: Volatile<u16>,
    pub pref_base_upper: Volatile<u32>,
    pub pref_limit_upper: Volatile<u32>,
    pub io_base_upper: Volatile<u16>,
    pub io_limit_upper: Volatile<u16>,
    pub cap_ptr: Volatile<u32>,
    pub exprom_base: Volatile<u32>,
    pub int_line: Volatile<u8>,
    pub int_pin: Volatile<u8>,
    pub bridge_control: Volatile<u16>,
}
