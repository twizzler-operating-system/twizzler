//! PCIe-specific functionality.

use std::ptr::NonNull;

pub use twizzler_abi::device::bus::pcie::*;
use twizzler_abi::{
    device::InterruptVector,
    kso::{KactionCmd, KactionError, KactionFlags},
};
use volatile::{
    access::{Access, ReadWrite, Readable},
    map_field, VolatilePtr, VolatileRef,
};

use crate::device::{events::InterruptAllocationError, Device, MmioObject};

pub struct PcieCapabilityIterator<'a> {
    _dev: &'a Device,
    cfg: &'a MmioObject,
    off: usize,
}

#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
#[repr(packed(4))]
pub struct PcieCapabilityHeader {
    pub id: u8,
    pub next: u8,
}

#[derive(Debug, Copy, Clone)]
#[repr(packed(4))]
pub struct MsiCapability {
    pub header: PcieCapabilityHeader,
    pub msg_ctrl: u16,
    pub msg_addr_low: u32,
    pub msg_addr_hi: u32,
    pub msg_data: u16,
    pub resv: u16,
    pub mask: u32,
    pub pending: u32,
}

#[derive(Debug, Copy, Clone)]
#[repr(packed(4))]
pub struct MsixCapability {
    pub header: PcieCapabilityHeader,
    pub msg_ctrl: u16,
    pub table_offset_and_bir: u32,
    pub pending_offset_and_bir: u32,
}

impl MsixCapability {
    fn get_table_info<'a, A: Readable + Access>(msix: VolatilePtr<'a, Self, A>) -> (u8, usize) {
        let info = map_field!(msix.table_offset_and_bir).read();
        ((info & 0x7) as u8, (info & !0x7) as usize)
    }

    fn table_len<'a, A: Readable + Access>(msix: VolatilePtr<'a, Self, A>) -> usize {
        (map_field!(msix.msg_ctrl).read() & 0x7ff) as usize
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(packed(8))]
pub struct MsixTableEntry {
    msg_addr_lo: u32,
    msg_addr_hi: u32,
    msg_data: u32,
    vec_ctrl: u32,
}

#[derive(Debug)]
pub enum PcieCapability<'a> {
    Unknown(u8),
    Msi(VolatileRef<'a, MsiCapability>),
    MsiX(VolatileRef<'a, MsixCapability>),
}

impl<'a> Iterator for PcieCapabilityIterator<'a> {
    type Item = PcieCapability<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.off == 0 {
            return None;
        }
        unsafe {
            let cap = self.cfg.get_mmio_offset::<PcieCapabilityHeader>(self.off);
            let cap = cap.as_ptr();
            let ret = match map_field!(cap.id).read() {
                5 => PcieCapability::Msi(self.cfg.get_mmio_offset_mut::<MsiCapability>(self.off)),
                0x11 => {
                    PcieCapability::MsiX(self.cfg.get_mmio_offset_mut::<MsixCapability>(self.off))
                }
                x => PcieCapability::Unknown(x),
            };
            self.off = (map_field!(cap.next).read() & 0xfc) as usize;
            Some(ret)
        }
    }
}

// TODO: allow for dest-ID and other options, and propegate all this stuff through the API.
fn calc_msg_info(vec: InterruptVector, level: bool) -> (u64, u32) {
    let addr = (0xfee << 20) | (0 << 12);
    let data: u32 = vec.into();
    let data = data | if level { 1 << 15 } else { 0 };
    (addr, data)
}

impl Device {
    fn pcie_capabilities<'a>(&'a self, mm: &'a MmioObject) -> Option<PcieCapabilityIterator<'a>> {
        let cfg = unsafe { mm.get_mmio_offset::<PcieDeviceHeader>(0) };
        let cfg = cfg.as_ptr();
        let ptr = map_field!(cfg.cap_ptr).read() & 0xfc;
        let hdr = map_field!(cfg.fnheader);
        if map_field!(hdr.status).read() & (1 << 4) == 0 {
            return None;
        }
        Some(PcieCapabilityIterator {
            _dev: self,
            cfg: mm,
            off: ptr as usize,
        })
    }

    fn find_mmio_bar(&self, bar: usize) -> Option<MmioObject> {
        let mut idx = 0;
        while let Some(mm) = self.get_mmio(idx) {
            if mm.get_info().info == bar as u64 {
                return Some(mm);
            }
            idx += 1;
        }
        None
    }

    fn allocate_msix_interrupt(
        &self,
        msix: volatile::VolatilePtr<'_, MsixCapability, ReadWrite>,
        vec: InterruptVector,
        inum: usize,
    ) -> Result<u32, InterruptAllocationError> {
        let (bar, offset) = MsixCapability::get_table_info(msix);
        map_field!(msix.msg_ctrl).write(1 << 15);
        let mmio = self
            .find_mmio_bar(bar.into())
            .ok_or(InterruptAllocationError::Unsupported)?;
        let table = unsafe {
            let start = mmio
                .get_mmio_offset::<MsixTableEntry>(offset)
                .as_ptr()
                .as_raw_ptr()
                .as_ptr();
            let len = MsixCapability::table_len(msix);
            VolatilePtr::new(NonNull::from(core::slice::from_raw_parts_mut(start, len)))
        };
        let (msg_addr, msg_data) = calc_msg_info(vec, false);
        let entry = table.index(inum);
        map_field!(entry.msg_addr_lo).write(msg_addr as u32);
        map_field!(entry.msg_addr_hi).write((msg_addr >> 32) as u32);
        map_field!(entry.msg_data).write(msg_data);
        map_field!(entry.vec_ctrl).write(0);
        Ok(inum as u32)
    }

    fn allocate_msi_interrupt(
        &self,
        _msi: &VolatilePtr<'_, MsiCapability, ReadWrite>,
        _vec: InterruptVector,
    ) -> Result<u32, InterruptAllocationError> {
        todo!()
    }

    fn allocate_pcie_interrupt(
        &self,
        vec: InterruptVector,
        inum: usize,
    ) -> Result<u32, InterruptAllocationError> {
        // Prefer MSI-X
        let mm = self.get_mmio(0).unwrap();
        for cap in self
            .pcie_capabilities(&mm)
            .ok_or(InterruptAllocationError::Unsupported)?
        {
            if let PcieCapability::MsiX(mut m) = cap {
                for msitest in self
                    .pcie_capabilities(&mm)
                    .ok_or(InterruptAllocationError::Unsupported)?
                {
                    if let PcieCapability::Msi(mut msi) = msitest {
                        let msi = msi.as_mut_ptr();
                        map_field!(msi.msg_ctrl).write(0);
                    }
                }
                return self.allocate_msix_interrupt(m.as_mut_ptr(), vec, inum);
            }
        }
        for cap in self
            .pcie_capabilities(&mm)
            .ok_or(InterruptAllocationError::Unsupported)?
        {
            if let PcieCapability::Msi(mut m) = cap {
                return self.allocate_msi_interrupt(&m.as_mut_ptr(), vec);
            }
        }
        Err(InterruptAllocationError::Unsupported)
    }

    pub(crate) fn allocate_interrupt(
        &self,
        inum: usize,
    ) -> Result<(InterruptVector, u32), InterruptAllocationError> {
        let vec = self
            .kaction(
                KactionCmd::Specific(PcieKactionSpecific::AllocateInterrupt.into()),
                0,
                KactionFlags::empty(),
                inum as u64,
            )
            .map_err(|e| InterruptAllocationError::KernelError(e))?;
        let vec = vec
            .unwrap_u64()
            .try_into()
            .map_err(|_| InterruptAllocationError::KernelError(KactionError::Unknown))?;
        let int = self.allocate_pcie_interrupt(vec, inum)?;
        Ok((vec, int))
    }
}
