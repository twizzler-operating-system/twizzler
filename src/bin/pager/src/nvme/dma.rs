use nvme::{ds::cmd::PrpListOrBuffer, hosted::memory::PhysicalPageCollection};
use twizzler_driver::dma::{DeviceSync, DmaPin, DmaPool, DmaRegion, DmaSliceRegion, DMA_PAGE_SIZE};

struct PrpMgr {
    list: Vec<DmaSliceRegion<u64>>,
    start: u64,
    embed_len: usize,
}

pub struct NvmeDmaRegion<T: DeviceSync> {
    reg: DmaRegion<T>,
    prp: Option<PrpMgr>,
}

impl<'a, T: DeviceSync> NvmeDmaRegion<T> {
    pub fn new(region: DmaRegion<T>) -> Self {
        Self {
            reg: region,
            prp: None,
        }
    }

    pub fn dma_region(&self) -> &DmaRegion<T> {
        &self.reg
    }
}

fn __get_prp_list_or_buffer(pin: DmaPin, prp_embed: &mut [u64], dma: &DmaPool) -> PrpMgr {
    let entries_per_page = DMA_PAGE_SIZE / 8;
    let pin_len = pin.len();
    let first_prp_page = dma.allocate_array(entries_per_page, 0u64).unwrap();
    let mut list = vec![first_prp_page];
    let mut pin_iter = pin.into_iter();
    for idx in 0..pin_len {
        if idx < prp_embed.len() {
            prp_embed[idx] = pin_iter.next().unwrap().addr().into();
            println!("prp embed num {}: {:?}", idx, prp_embed[idx]);
        } else {
            break;
        }
    }

    for (num, page) in pin_iter.enumerate() {
        println!("prp num {}: {:?}", num, page);
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
        embed_len: prp_embed.len(),
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for &'a mut NvmeDmaRegion<T> {
    fn get_prp_list_or_buffer(
        &mut self,
        prp_embed: &mut [u64],
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        let pin = self.reg.pin().unwrap();
        if pin.len() == 1 {
            return Some(PrpListOrBuffer::Buffer(
                pin.into_iter().next().unwrap().addr().into(),
            ));
        }
        if let Some(ref prp) = self.prp {
            if prp.embed_len == prp_embed.len() {
                return Some(PrpListOrBuffer::PrpList(prp.start));
            }
        }
        let prp = __get_prp_list_or_buffer(pin, prp_embed, dma);
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

pub struct NvmeDmaSliceRegion<T: DeviceSync> {
    reg: DmaSliceRegion<T>,
    prp: Option<PrpMgr>,
}

impl<'a, T: DeviceSync> NvmeDmaSliceRegion<T> {
    pub fn new(region: DmaSliceRegion<T>) -> Self {
        Self {
            reg: region,
            prp: None,
        }
    }

    pub fn dma_region(&self) -> &DmaSliceRegion<T> {
        &self.reg
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for &'a mut NvmeDmaSliceRegion<T> {
    fn get_prp_list_or_buffer(
        &mut self,
        prp_embed: &mut [u64],
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        let pin = self.reg.pin().unwrap();
        if pin.len() == 1 {
            return Some(PrpListOrBuffer::Buffer(
                pin.into_iter().next().unwrap().addr().into(),
            ));
        }
        if let Some(ref prp) = self.prp {
            if prp.embed_len == prp_embed.len() {
                return Some(PrpListOrBuffer::PrpList(prp.start));
            }
        }
        let prp = __get_prp_list_or_buffer(pin, prp_embed, dma);
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
