use nvme::{
    ds::cmd::PrpListOrBuffer,
    hosted::memory::{PhysicalPageCollection, PrpMode},
};
use twizzler_driver::dma::{DeviceSync, DmaPin, DmaPool, DmaRegion, DmaSliceRegion, DMA_PAGE_SIZE};

struct PrpMgr {
    _list: Vec<DmaSliceRegion<u64>>,
    mode: PrpMode,
    buffer: bool,
    embed: [u64; 2],
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

    pub fn dma_region_mut(&mut self) -> &mut DmaRegion<T> {
        &mut self.reg
    }
}

fn __get_prp_list_or_buffer2(pin: DmaPin, dma: &DmaPool, mode: PrpMode) -> PrpMgr {
    let entries_per_page = DMA_PAGE_SIZE / 8;
    let pin_len = pin.len();
    let mut pin_iter = pin.into_iter();

    let prp = match pin_len {
        1 => PrpMgr {
            _list: vec![],
            embed: [pin_iter.next().unwrap().addr().into(), 0],
            mode,
            buffer: true,
        },
        2 if mode == PrpMode::Double => PrpMgr {
            _list: vec![],
            embed: [
                pin_iter.next().unwrap().addr().into(),
                pin_iter.next().unwrap().addr().into(),
            ],
            mode,
            buffer: false,
        },
        _ => {
            let mut first_prp_page = dma.allocate_array(entries_per_page, 0u64).unwrap();
            let start = first_prp_page
                .pin()
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
                .addr()
                .into();
            let mut list = vec![first_prp_page];
            let embed = [pin_iter.next().unwrap().addr().into(), start];
            for (num, page) in pin_iter.enumerate() {
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
            PrpMgr {
                _list: list,
                mode,
                embed,
                buffer: false,
            }
        }
    };
    prp
}

impl PrpMgr {
    fn prp_list_or_buffer(&self) -> PrpListOrBuffer {
        match self.mode {
            PrpMode::Double => {
                if self.buffer {
                    PrpListOrBuffer::Buffer(self.embed[0])
                } else {
                    PrpListOrBuffer::PrpFirstAndList(self.embed[0], self.embed[1])
                }
            }
            PrpMode::Single => {
                if self.buffer {
                    PrpListOrBuffer::Buffer(self.embed[0])
                } else {
                    PrpListOrBuffer::PrpList(self.embed[0])
                }
            }
        }
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for &'a mut NvmeDmaRegion<T> {
    fn get_prp_list_or_buffer(
        &mut self,
        mode: PrpMode,
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        if let Some(ref prp) = self.prp {
            if mode == prp.mode {
                return Some(prp.prp_list_or_buffer());
            }
        }

        let pin = self.reg.pin().unwrap();
        self.prp = Some(__get_prp_list_or_buffer2(pin, dma, mode));
        Some(self.prp.as_ref().unwrap().prp_list_or_buffer())
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

    pub fn dma_region_mut(&mut self) -> &mut DmaSliceRegion<T> {
        &mut self.reg
    }
}

impl<'a, T: DeviceSync> PhysicalPageCollection for &'a mut NvmeDmaSliceRegion<T> {
    fn get_prp_list_or_buffer(
        &mut self,
        mode: PrpMode,
        dma: Self::DmaType,
    ) -> Option<nvme::ds::cmd::PrpListOrBuffer> {
        if let Some(ref prp) = self.prp {
            if mode == prp.mode {
                return Some(prp.prp_list_or_buffer());
            }
        }

        let pin = self.reg.pin().unwrap();
        self.prp = Some(__get_prp_list_or_buffer2(pin, dma, mode));
        Some(self.prp.as_ref().unwrap().prp_list_or_buffer())
    }

    type DmaType = &'a DmaPool;
}
