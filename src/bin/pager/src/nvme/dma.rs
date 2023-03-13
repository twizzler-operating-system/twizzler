use nvme::{ds::cmd::PrpListOrBuffer, hosted::memory::PhysicalPageCollection};
use twizzler_driver::dma::{DeviceSync, DmaPin, DmaPool, DmaRegion, DmaSliceRegion, DMA_PAGE_SIZE};

struct PrpMgr<'a> {
    list: Vec<DmaSliceRegion<'a, u64>>,
    start: u64,
}

pub struct NvmeDmaRegion<'a, T: DeviceSync> {
    reg: DmaRegion<'a, T>,
    prp: Option<PrpMgr<'a>>,
}

impl<'a, T: DeviceSync> NvmeDmaRegion<'a, T> {
    pub fn new(region: DmaRegion<'a, T>) -> Self {
        Self {
            reg: region,
            prp: None,
        }
    }

    pub fn dma_region(&self) -> &DmaRegion<'_, T> {
        &self.reg
    }
}

fn __get_prp_list_or_buffer<'a>(pin: DmaPin, dma: &'a DmaPool) -> PrpMgr<'a> {
    let entries_per_page = DMA_PAGE_SIZE / 8;
    let pin_len = pin.len();
    let first_prp_page = dma.allocate_array(entries_per_page, 0u64).unwrap();
    let mut list = vec![first_prp_page];
    for (num, page) in pin.into_iter().enumerate() {
        let index = num % entries_per_page;
        if (num + 1) % entries_per_page == 0 && num != pin_len - 1 {
            // Last entry with more to record, chain.
            let mut next_prp_page = dma.allocate_array(entries_per_page, 0u64).unwrap();
            let pin = next_prp_page.pin().unwrap();
            assert_eq!(pin.len(), 1);
            let phys = pin.into_iter().next().unwrap().addr();
            list.last_mut()
                .unwrap()
                .with_mut(index..(index + 1), |array: &mut [u64]| {
                    array[0] = phys.into();
                });

            list.push(next_prp_page);
        }

        list.last_mut()
            .unwrap()
            .with_mut(index..(index + 1), |array: &mut [u64]| {
                array[0] = page.addr().into();
            });
    }
    let first_prp_addr = list[0].pin().unwrap().into_iter().next().unwrap().addr();
    PrpMgr {
        list,
        start: first_prp_addr.into(),
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for NvmeDmaRegion<'a, T> {
    fn get_prp_list_or_buffer(
        &mut self,
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        let pin = self.reg.pin().unwrap();
        if pin.len() == 1 {
            return Some(PrpListOrBuffer::Buffer(
                pin.into_iter().next().unwrap().addr().into(),
            ));
        }
        let prp = __get_prp_list_or_buffer(pin, dma);
        self.prp = Some(prp);
        Some(PrpListOrBuffer::PrpList(self.prp.as_ref().unwrap().start))
    }

    fn get_dptr(&mut self, _sgl_allowed: bool) -> Option<nvme::ds::queue::subentry::Dptr> {
        let pin = self.reg.pin().unwrap();
        Some(nvme::ds::queue::subentry::Dptr::Prp(
            pin[0].addr().into(),
            0,
        ))
    }

    type DmaType = &'a DmaPool;
}

pub struct NvmeDmaSliceRegion<'a, T: DeviceSync> {
    reg: DmaSliceRegion<'a, T>,
    prp: Option<PrpMgr<'a>>,
}

impl<'a, T: DeviceSync> NvmeDmaSliceRegion<'a, T> {
    pub fn new(region: DmaSliceRegion<'a, T>) -> Self {
        Self {
            reg: region,
            prp: None,
        }
    }

    pub fn dma_region(&self) -> &DmaSliceRegion<'_, T> {
        &self.reg
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for NvmeDmaSliceRegion<'a, T> {
    fn get_prp_list_or_buffer(
        &mut self,
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        let pin = self.reg.pin().unwrap();
        if pin.len() == 1 {
            return Some(PrpListOrBuffer::Buffer(
                pin.into_iter().next().unwrap().addr().into(),
            ));
        }
        let prp = __get_prp_list_or_buffer(pin, dma);
        self.prp = Some(prp);
        Some(PrpListOrBuffer::PrpList(self.prp.as_ref().unwrap().start))
    }

    fn get_dptr(&mut self, _sgl_allowed: bool) -> Option<nvme::ds::queue::subentry::Dptr> {
        let pin = self.reg.pin().unwrap();
        Some(nvme::ds::queue::subentry::Dptr::Prp(
            pin[0].addr().into(),
            0,
        ))
    }

    type DmaType = &'a DmaPool;
}
