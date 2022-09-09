use nvme::hosted::memory::PhysicalPageCollection;
use twizzler_driver::dma::{DeviceSync, DmaRegion};

pub struct NvmeDmaRegion<T: DeviceSync>(DmaRegion<T>);

impl<T: DeviceSync> PhysicalPageCollection for NvmeDmaRegion<T> {
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

impl<T: DeviceSync> NvmeDmaRegion<T> {
    pub fn new(inner: DmaRegion<T>) -> Self {
        Self(inner)
    }

    pub fn into_dma_reg(self) -> DmaRegion<T> {
        self.0
    }
}
