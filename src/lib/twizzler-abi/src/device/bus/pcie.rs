use crate::{kso::KactionError, vcell::Volatile};
#[allow(dead_code)]
#[repr(C)]
pub struct PcieInfo {
    pub bus_start: u8,
    pub bus_end: u8,
    pub seg_nr: u16,
}

#[repr(u32)]
pub enum PcieKactionSpecific {
    RegisterDevice = 0,
    AllocateInterrupt = 1,
}

impl TryFrom<u32> for PcieKactionSpecific {
    type Error = KactionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => PcieKactionSpecific::RegisterDevice,
            1 => PcieKactionSpecific::AllocateInterrupt,
            _ => Err(KactionError::InvalidArgument)?,
        })
    }
}

#[allow(dead_code)]
#[repr(packed)]
pub struct PcieFunctionHeader {
    vendor_id: Volatile<u16>,
    device_id: Volatile<u16>,
    command: Volatile<u16>,
    status: Volatile<u16>,
    revision: Volatile<u8>,
    progif: Volatile<u8>,
    subclass: Volatile<u8>,
    class: Volatile<u8>,
    cache_line_size: Volatile<u8>,
    latency_timer: Volatile<u8>,
    header_type: Volatile<u8>,
    bist: Volatile<u8>,
}

#[allow(dead_code)]
#[repr(packed)]
pub struct PcieDeviceHeader {
    fnheader: PcieFunctionHeader,
    bars: [Volatile<u32>; 6],
    cardbus_cis_ptr: Volatile<u32>,
    subsystem_vendor_id: Volatile<u16>,
    subsystem_id: Volatile<u16>,
    exprom_base: Volatile<u32>,
    cap_ptr: Volatile<u32>,
    res0: Volatile<u32>,
    int_line: Volatile<u8>,
    int_pin: Volatile<u8>,
    min_grant: Volatile<u8>,
    max_latency: Volatile<u8>,
}

#[allow(dead_code)]
#[repr(packed)]
pub struct PcieBridgeHeader {
    fnheader: PcieFunctionHeader,
    bar: [Volatile<u32>; 2],
    primary_bus_nr: Volatile<u8>,
    secondary_bus_nr: Volatile<u8>,
    subordinate_bus_nr: Volatile<u8>,
    secondary_latency_timer: Volatile<u8>,
    io_base: Volatile<u8>,
    io_limit: Volatile<u8>,
    secondary_status: Volatile<u8>,
    memory_base: Volatile<u16>,
    memory_limit: Volatile<u16>,
    pref_memory_base: Volatile<u16>,
    pref_memory_limit: Volatile<u16>,
    pref_base_upper: Volatile<u32>,
    pref_limit_upper: Volatile<u32>,
    io_base_upper: Volatile<u16>,
    io_limit_upper: Volatile<u16>,
    cap_ptr: Volatile<u32>,
    exprom_base: Volatile<u32>,
    int_line: Volatile<u8>,
    int_pin: Volatile<u8>,
    bridge_control: Volatile<u16>,
}
