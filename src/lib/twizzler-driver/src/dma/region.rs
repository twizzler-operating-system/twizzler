use core::marker::PhantomData;
use core::ops::Range;

use super::{Access, DeviceSync, DmaObject, DmaPin, SyncMode};

pub struct DmaRegion<'a, T: DeviceSync> {
    virt: *mut u8,
    pin: Option<DmaPin<'a>>,
    len: usize,
    access: Access,
    mem_init: bool,
    obj: &'a DmaObject,
    _pd: PhantomData<T>,
}

pub struct DmaArrayRegion<'a, T: DeviceSync> {
    region: DmaRegion<'a, T>,
    len: usize,
}

impl<'a, T: DeviceSync> DmaRegion<'a, T> {
    pub fn num_bytes(&self) -> usize {
        todo!()
    }

    pub fn access(&self) -> Access {
        todo!()
    }
    // Determines the backing information for region. This includes acquiring physical addresses for
    // the region and holding a pin for the pages.
    fn pin(&mut self) -> DmaPin<'a> {
        todo!()
    }

    // Synchronize the region for cache coherence.
    fn sync(&mut self, sync: SyncMode) {
        todo!()
    }

    // Run a closure that takes a reference to the DMA data, ensuring coherence.
    fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        todo!()
    }

    // Run a closure that takes a mutable reference to the DMA data, ensuring coherence.
    fn with_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        todo!()
    }

    /// Release any pin created for this region.
    ///
    /// # Safety
    /// Caller must ensure that no device is using the information from any active pins for this region.
    pub unsafe fn release_pin(&self) {
        todo!()
    }

    /// Get a reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    unsafe fn get(&self) -> &T {
        todo!()
    }

    /// Get a mutable reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    unsafe fn get_mut(&self) -> &mut T {
        todo!()
    }
}

impl<'a, T: DeviceSync> DmaArrayRegion<'a, T> {
    pub fn num_bytes(&self) -> usize {
        todo!()
    }

    pub fn access(&self) -> Access {
        todo!()
    }

    pub fn len(&self) -> usize {
        todo!()
    }
    // Determines the backing information for region. This includes acquiring physical addresses for
    // the region and holding a pin for the pages.
    fn pin(&mut self) -> DmaPin<'a> {
        todo!()
    }

    // Synchronize the region for cache coherence.
    fn sync(&mut self, sync: SyncMode) {
        todo!()
    }

    // Run a closure that takes a reference to the DMA data, ensuring coherence.
    fn with<F, R>(&self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&[T]) -> R,
    {
        todo!()
    }

    // Run a closure that takes a mutable reference to the DMA data, ensuring coherence.
    fn with_mut<F, R>(&mut self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&mut [T]) -> R,
    {
        todo!()
    }

    /// Release any pin created for this region.
    ///
    /// # Safety
    /// Caller must ensure that no device is using the information from any active pins for this region.
    pub unsafe fn release_pin(&self) {
        todo!()
    }

    /// Get a reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    unsafe fn get(&self) -> &[T] {
        todo!()
    }

    /// Get a mutable reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    unsafe fn get_mut(&self) -> &mut [T] {
        todo!()
    }
}

impl<'a, T: DeviceSync> Drop for DmaRegion<'a, T> {
    fn drop(&mut self) {
        todo!()
    }
}

impl<'a, T: DeviceSync> Drop for DmaArrayRegion<'a, T> {
    fn drop(&mut self) {
        todo!()
    }
}
