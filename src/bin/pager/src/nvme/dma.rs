use nvme::hosted::memory::PhysicalPageCollection;
use twizzler_driver::dma::{DeviceSync, DmaRegion};

pub struct NvmeDmaRegion<'a, T: DeviceSync>(DmaRegion<'a, T>);

impl<'a, T: DeviceSync> NvmeDmaRegion<'a, T> {
    pub fn new(region: DmaRegion<'a, T>) -> Self {
        Self(region)
    }

    pub fn dma_region(&self) -> &DmaRegion<'_, T> {
        &self.0
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for NvmeDmaRegion<'a, T> {
    fn get_prp_list_or_buffer(&mut self) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        todo!()
    }

    fn get_dptr(&mut self, _sgl_allowed: bool) -> Option<nvme::ds::queue::subentry::Dptr> {
        let pin = self.0.pin().unwrap();
        Some(nvme::ds::queue::subentry::Dptr::Prp(
            pin[0].addr().into(),
            0,
        ))
    }
}
