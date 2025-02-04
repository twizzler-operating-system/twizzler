#[cfg(any(feature = "kernel"))]
use core::{mem::size_of, ptr::NonNull};

#[cfg(any(feature = "kernel"))]
use volatile::VolatilePtr;

use crate::kso::KactionError;
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
#[repr(C, packed(4))]
#[derive(Copy, Clone, Debug)]
pub struct PcieFunctionHeader {
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision: u8,
    pub progif: u8,
    pub subclass: u8,
    pub class: u8,
    pub cache_line_size: u8,
    pub latency_timer: u8,
    pub header_type: u8,
    pub bist: u8,
}

/// The standard PCIe device header.
/// See the PCI spec for more details.
#[allow(dead_code)]
#[repr(C, packed(8))]
#[derive(Copy, Clone)]
pub struct PcieDeviceHeader {
    pub fnheader: PcieFunctionHeader,
    pub bar0: u32,
    pub bar1: u32,
    pub bar2: u32,
    pub bar3: u32,
    pub bar4: u32,
    pub bar5: u32,
    pub cardbus_cis_ptr: u32,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub exprom_base: u32,
    pub cap_ptr: u32,
    res0: u32,
    pub int_line: u8,
    pub int_pin: u8,
    pub min_grant: u8,
    pub max_latency: u8,
}

/// The standard PCIe bridge header.
/// See the PCI spec for more details.
#[allow(dead_code)]
#[repr(C, packed(4096))]
#[derive(Copy, Clone, Debug)]
pub struct PcieBridgeHeader {
    pub fnheader: PcieFunctionHeader,
    pub bar0: u32,
    pub bar1: u32,
    pub primary_bus_nr: u8,
    pub secondary_bus_nr: u8,
    pub subordinate_bus_nr: u8,
    pub secondary_latency_timer: u8,
    pub io_base: u8,
    pub io_limit: u8,
    pub secondary_status: u8,
    pub memory_base: u16,
    pub memory_limit: u16,
    pub pref_memory_base: u16,
    pub pref_memory_limit: u16,
    pub pref_base_upper: u32,
    pub pref_limit_upper: u32,
    pub io_base_upper: u16,
    pub io_limit_upper: u16,
    pub cap_ptr: u32,
    pub exprom_base: u32,
    pub int_line: u8,
    pub int_pin: u8,
    pub bridge_control: u16,
}

#[cfg(any(feature = "kernel"))]
pub fn get_bar(cfg: VolatilePtr<'_, PcieDeviceHeader>, n: usize) -> VolatilePtr<'_, u32> {
    unsafe {
        cfg.map(|mut x| {
            let ptr = (x.as_mut() as *mut _ as *mut u32)
                .byte_add(size_of::<PcieFunctionHeader>() + size_of::<u32>() * n);
            NonNull::new(ptr).unwrap()
        })
    }
}

#[derive(Copy, Clone)]
#[allow(dead_code)]
#[repr(C, packed)]
pub struct PcieCapabilityHeader {
    pub id: u8,
    pub next: u8,
}


