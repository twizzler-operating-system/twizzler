use std::ptr::NonNull;

pub use twizzler_abi::device::bus::pcie::*;
use twizzler_abi::{
    device::InterruptVector,
    kso::{KactionCmd, KactionError, KactionFlags},
    vcell::Volatile,
};

use crate::device::{interrupts::InterruptAllocationError, mmio::MmioObject, Device};

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
    pub header: PcieCapabilityHeader,
    pub msg_ctrl: Volatile<u16>,
    pub msg_addr_low: Volatile<u32>,
    pub msg_addr_hi: Volatile<u32>,
    pub msg_data: Volatile<u16>,
    pub resv: u16,
    pub mask: Volatile<u32>,
    pub pending: Volatile<u32>,
}

#[allow(unaligned_references)]
#[derive(Debug)]
#[repr(packed)]
pub struct MsixCapability {
    pub header: PcieCapabilityHeader,
    pub msg_ctrl: Volatile<u16>,
    pub table_offset_and_bir: Volatile<u32>,
    pub pending_offset_and_bir: Volatile<u32>,
}

impl MsixCapability {
#[allow(unaligned_references)]
    fn get_table_info(&self) -> (u8, usize) {
        let info = self.table_offset_and_bir.get();
        ((info & 0x7) as u8, (info & !0x7) as usize)
    }

#[allow(unaligned_references)]
    fn table_len(&self) -> usize {
        (self.msg_ctrl.get() & 0x7ff) as usize
    }
}

#[allow(unaligned_references)]
#[derive(Debug)]
#[repr(packed)]
pub struct MsixTableEntry {
    msg_addr_lo: Volatile<u32>,
    msg_addr_hi: Volatile<u32>,
    msg_data: Volatile<u32>,
    vec_ctrl: Volatile<u32>,
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

fn calc_msg_info(vec: InterruptVector) -> (u64, u32) {
    todo!()
}

impl Device {
    #[allow(unaligned_references)]
    fn pcie_capabilities(&self) -> Option<PcieCapabilityIterator> {
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

#[allow(unaligned_references)]
    fn allocate_msix_interrupt(
        &self,
        msix: &MsixCapability,
        vec: InterruptVector,
        inum: usize,
    ) -> Result<u32, InterruptAllocationError> {
        let (bar, offset) = msix.get_table_info();
        let mmio = self
            .find_mmio_bar(bar.into())
            .ok_or(InterruptAllocationError::Unsupported)?;
        let table = unsafe {
            let start = mmio.get_mmio_offset::<MsixTableEntry>(offset) as *const MsixTableEntry
                as *mut MsixTableEntry;
            let len = msix.table_len();
            core::slice::from_raw_parts_mut(start, len)
        };
        let (msg_addr, msg_data) = calc_msg_info(vec);
        table[inum].msg_addr_lo.set(msg_addr as u32);
        table[inum].msg_addr_hi.set((msg_addr >> 32) as u32);
        table[inum].msg_data.set(msg_data);
        table[inum].vec_ctrl.set(0);
        Ok(inum as u32)
    }

    fn allocate_msi_interrupt(
        &self,
        msix: &MsiCapability,
        vec: InterruptVector,
    ) -> Result<u32, InterruptAllocationError> {
        todo!()
    }

    fn allocate_pcie_interrupt(
        &self,
        vec: InterruptVector,
        inum: usize,
    ) -> Result<u32, InterruptAllocationError> {
        // Prefer MSI-X
        for cap in self
            .pcie_capabilities()
            .ok_or(InterruptAllocationError::Unsupported)?
        {
            if let PcieCapability::MsiX(m) = cap {
                return unsafe { self.allocate_msix_interrupt(m.as_ref(), vec, inum) };
            }
        }
        for cap in self
            .pcie_capabilities()
            .ok_or(InterruptAllocationError::Unsupported)?
        {
            if let PcieCapability::Msi(m) = cap {
                return unsafe { self.allocate_msi_interrupt(m.as_ref(), vec) };
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
