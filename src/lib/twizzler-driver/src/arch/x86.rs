use crate::dma::{DeviceSync, DmaRegion};

pub(crate) fn sync<'a, T: DeviceSync>(_region: &DmaRegion<'a, T>) {
    // x86 is already coherent
}
