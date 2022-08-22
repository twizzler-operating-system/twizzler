use std::sync::atomic::Ordering;

use crate::dma::{DeviceSync, DmaRegion, SyncMode};

pub(crate) fn sync<'a, T: DeviceSync>(
    _region: &DmaRegion<'a, T>,
    _mode: SyncMode,
    _offset: usize,
    _len: usize,
) {
    core::sync::atomic::fence(Ordering::SeqCst);
    // x86 is already coherent
}

pub const DMA_PAGE_SIZE: usize = 0x1000;
