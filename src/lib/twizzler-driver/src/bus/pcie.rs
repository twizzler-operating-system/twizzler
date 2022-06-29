use std::ptr::NonNull;

pub use twizzler_abi::device::bus::pcie::*;
use twizzler_abi::vcell::Volatile;

use crate::device::{Device, MmioObject};

pub struct PcieCapabilityIterator {
    cfg: MmioObject,
    off: usize,
}

#[derive(Debug)]
#[allow(dead_code)]
#[repr(packed)]
pub struct PcieCapabilityHeader {
    pub id: u8,
    pub next: u8,
}

#[allow(unaligned_references)]
#[derive(Debug)]
#[repr(packed)]
pub struct MsiCapability {
    header: PcieCapabilityHeader,
    msg_ctrl: Volatile<u16>,
    msg_addr_low: Volatile<u32>,
    msg_addr_hi: Volatile<u32>,
    msg_data: Volatile<u16>,
    resv: u16,
    mask: Volatile<u32>,
    pending: Volatile<u32>,
}

#[allow(unaligned_references)]
#[derive(Debug)]
#[repr(packed)]
pub struct MsixCapability {
    header: PcieCapabilityHeader,
    msg_ctrl: Volatile<u16>,
    table_offset_and_bir: Volatile<u32>,
    pending_offset_and_bir: Volatile<u32>,
}

#[derive(Debug)]
pub enum PcieCapability {
    Unknown(u8),
    Msi(NonNull<MsiCapability>),
    MsiX(NonNull<MsixCapability>),
}

impl Iterator for PcieCapabilityIterator {
    type Item = PcieCapability;

    fn next(&mut self) -> Option<Self::Item> {
        if self.off == 0 {
            return None;
        }
        unsafe {
            let cap = self.cfg.get_mmio_offset::<PcieCapabilityHeader>(self.off);
            let ret = match cap.id {
                5 => {
                    PcieCapability::Msi(self.cfg.get_mmio_offset::<MsiCapability>(self.off).into())
                }
                0x11 => PcieCapability::MsiX(
                    self.cfg.get_mmio_offset::<MsixCapability>(self.off).into(),
                ),
                x => PcieCapability::Unknown(x),
            };
            self.off = (cap.next & 0xfc) as usize;
            Some(ret)
        }
    }
}

impl Device {
    #[allow(unaligned_references)]
    pub fn pcie_capabilities(&self) -> Option<PcieCapabilityIterator> {
        let mm = self.get_mmio(0)?;
        let cfg = unsafe { mm.get_mmio_offset::<PcieDeviceHeader>(0) };
        let ptr = cfg.cap_ptr.get() & 0xfc;
        if cfg.fnheader.status.get() & (1 << 4) == 0 {
            return None;
        }
        Some(PcieCapabilityIterator {
            cfg: mm,
            off: ptr as usize,
        })
    }
}
