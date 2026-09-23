use crate::dma::{DeviceSync, DmaRegion, SyncMode};

pub(crate) fn sync<T: DeviceSync>(
    _region: &DmaRegion<T>,
    _mode: SyncMode,
    _offset: usize,
    _len: usize,
) {
    // The PCIe hosts we run on are cache-coherent; order the CPU's writes before the doorbell.
    unsafe { core::arch::asm!("dsb sy") };
}

/// Size of a page for this DMA system.
pub const DMA_PAGE_SIZE: usize = 0x1000;
