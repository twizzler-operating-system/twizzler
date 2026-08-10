use std::sync::{Arc, Mutex};

use nvme::{
    ds::cmd::PrpListOrBuffer,
    hosted::memory::{PhysicalPageCollection, PrpMode},
};
use twizzler_driver::dma::{
    DeviceSync, DmaPool, DmaRegion, DmaSliceRegion, PhysInfo, DMA_PAGE_SIZE,
};

pub struct PrpMgr {
    _list: Vec<DmaSliceRegion<u64>>,
    mode: PrpMode,
    dma: Arc<CachedDmaPool>,
    buffer: bool,
    embed: [u64; 2],
}

pub struct NvmeDmaRegion<T: DeviceSync> {
    reg: DmaRegion<T>,
    prp: Option<PrpMgr>,
}

pub struct CachedDmaPool {
    pub dma: DmaPool,
    reuse: Mutex<Vec<DmaSliceRegion<u64>>>,
    reuse_pages: Mutex<Vec<DmaRegion<[u8; DMA_PAGE_SIZE]>>>,
}

impl CachedDmaPool {
    pub fn new(dma: DmaPool) -> Self {
        Self {
            dma,
            reuse: Mutex::new(Vec::new()),
            reuse_pages: Mutex::new(Vec::new()),
        }
    }

    pub fn get_page(&self) -> Option<DmaRegion<[u8; DMA_PAGE_SIZE]>> {
        if let Ok(mut reuse) = self.reuse_pages.try_lock() {
            if !reuse.is_empty() {
                return Some(reuse.pop().unwrap());
            }
        }
        self.dma.allocate([0u8; DMA_PAGE_SIZE]).ok()
    }

    pub fn put_page(&self, page: DmaRegion<[u8; DMA_PAGE_SIZE]>) {
        self.reuse_pages.lock().unwrap().push(page);
    }

    fn get(&self) -> Option<DmaSliceRegion<u64>> {
        if let Ok(mut reuse) = self.reuse.try_lock() {
            if !reuse.is_empty() {
                return Some(reuse.pop().unwrap());
            }
        }
        self.dma.allocate_array(DMA_PAGE_SIZE / 8, 0u64).ok()
    }

    fn put(&self, region: DmaSliceRegion<u64>) {
        self.reuse.lock().unwrap().push(region);
    }
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

    pub fn into_inner(self) -> DmaRegion<T> {
        self.reg
    }
}

pub fn get_prp_list_or_buffer(pin: &[PhysInfo], dma: &Arc<CachedDmaPool>, mode: PrpMode) -> PrpMgr {
    let entries_per_page = DMA_PAGE_SIZE / 8;
    let pin_len = pin.len();
    let mut pin_iter = pin.into_iter();

    let prp = match pin_len {
        1 => PrpMgr {
            _list: vec![],
            embed: [pin_iter.next().unwrap().into(), 0],
            dma: dma.clone(),
            mode,
            buffer: true,
        },
        2 if mode == PrpMode::Double => PrpMgr {
            _list: vec![],
            dma: dma.clone(),
            embed: [
                pin_iter.next().unwrap().into(),
                pin_iter.next().unwrap().into(),
            ],
            mode,
            buffer: false,
        },
        _ => {
            // The first data page rides in the command itself; the rest go in a chain of PRP list
            // pages. Every page but the last spends its final entry on a pointer to the next, so
            // only the last one gets to use all `entries_per_page` slots for data.
            let embed_first: u64 = pin[0].into();
            let rest = &pin[1..];
            let per_page = if rest.len() <= entries_per_page {
                entries_per_page
            } else {
                entries_per_page - 1
            };

            // Built back to front, so each page already knows the address it has to chain to.
            let mut list = Vec::with_capacity(rest.len().div_ceil(per_page));
            let mut next: Option<u64> = None;
            for chunk in rest.chunks(per_page).rev() {
                let mut page = dma.get().unwrap();
                let pin = page.pin().unwrap();
                assert_eq!(pin.len(), 1);
                let addr: u64 = pin.into_iter().next().unwrap().addr().into();

                // One `with_mut` for the whole page, rather than one per entry.
                let fill = chunk.len() + next.is_some() as usize;
                page.with_mut(0..fill, |array: &mut [u64]| {
                    for (slot, phys) in array.iter_mut().zip(chunk.iter()) {
                        *slot = (*phys).into();
                    }
                    if let Some(next) = next {
                        array[chunk.len()] = next;
                    }
                });

                next = Some(addr);
                list.push(page);
            }

            PrpMgr {
                _list: list,
                dma: dma.clone(),
                mode,
                embed: [embed_first, next.unwrap()],
                buffer: false,
            }
        }
    };
    prp
}

impl Drop for PrpMgr {
    fn drop(&mut self) {
        for d in self._list.drain(..) {
            self.dma.put(d);
        }
    }
}

impl PrpMgr {
    pub fn prp_list_or_buffer(&self) -> PrpListOrBuffer {
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
        self.prp = Some(get_prp_list_or_buffer(pin.backing, dma, mode));
        Some(self.prp.as_ref().unwrap().prp_list_or_buffer())
    }

    type DmaType = &'a Arc<CachedDmaPool>;
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
        self.prp = Some(get_prp_list_or_buffer(pin.backing, dma, mode));
        Some(self.prp.as_ref().unwrap().prp_list_or_buffer())
    }

    type DmaType = &'a Arc<CachedDmaPool>;
}
