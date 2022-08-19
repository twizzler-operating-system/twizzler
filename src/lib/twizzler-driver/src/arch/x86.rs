use crate::dma::{DeviceSync, DmaRegion, SyncMode};

pub(crate) fn sync<'a, T: DeviceSync>(
    _region: &DmaRegion<'a, T>,
    _mode: SyncMode,
    _offset: usize,
    _len: usize,
) {
    // x86 is already coherent
}

pub(crate) const DMA_PAGE_SIZE: usize = 0x1000;
