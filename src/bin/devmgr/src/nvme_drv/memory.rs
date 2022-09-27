use std::sync::{Arc, Weak};

use nvme::hosted::memory::PhysicalPageCollection;
use twizzler_driver::dma::{DeviceSync, DmaRegion};

use super::controller::{NvmeController, NvmeControllerRef};

pub struct NvmeDmaRegion<T: DeviceSync> {
    reg: DmaRegion<T>,
    ctrl: Weak<NvmeController>,
}

impl<T: DeviceSync> PhysicalPageCollection for NvmeDmaRegion<T> {
    fn get_prp_list_or_buffer(&mut self) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        todo!()
    }

    fn get_dptr(&mut self, _sgl_allowed: bool) -> Option<nvme::ds::queue::subentry::Dptr> {
        match self.nr_pages() {
            0 => None,
            1 => {
                let pin = self.reg.pin().unwrap();
                Some(nvme::ds::queue::subentry::Dptr::Prp(
                    pin[0].addr().into(),
                    0,
                ))
            }
            2 => {
                let pin = self.reg.pin().unwrap();
                Some(nvme::ds::queue::subentry::Dptr::Prp(
                    pin[0].addr().into(),
                    pin[1].addr().into(),
                ))
            }
            _ => self.build_prp_list(),
        }
    }
}

impl<T: DeviceSync> NvmeDmaRegion<T> {
    pub fn new(reg: DmaRegion<T>, ctrl: &NvmeControllerRef) -> Self {
        let ctrl = Arc::downgrade(ctrl);
        Self { reg, ctrl }
    }

    pub fn into_dma_reg(self) -> DmaRegion<T> {
        self.reg
    }

    fn nr_pages(&self) -> usize {
        self.reg.nr_pages()
    }

    fn build_prp_list(&mut self) -> Option<nvme::ds::queue::subentry::Dptr> {
        todo!()
    }
}
