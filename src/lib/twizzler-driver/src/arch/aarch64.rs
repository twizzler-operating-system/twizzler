use crate::dma::{DeviceSync, DmaRegion, SyncMode};

pub(crate) fn sync<T: DeviceSync>(
    _region: &DmaRegion<T>,
    _mode: SyncMode,
    _offset: usize,
    _len: usize,
) {
    todo!("sync")
}

// TODO: DMA page size.

/// Size of a page for this DMA system.
pub const DMA_PAGE_SIZE: usize = 0x1000;
