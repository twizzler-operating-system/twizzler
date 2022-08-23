use core::marker::PhantomData;
use core::ops::Range;
use std::sync::Arc;

use twizzler_abi::{
    kso::{
        pack_kaction_pin_start_and_len, unpack_kaction_pin_token_and_len, KactionCmd, KactionFlags,
        KactionGenericCmd,
    },
    syscall::{sys_kaction, PinnedPage},
};

use crate::arch::DMA_PAGE_SIZE;

use super::{
    pin::{PhysInfo, PinError},
    pool::{AllocatableDmaObject, SplitPageRange},
    Access, DeviceSync, DmaObject, DmaOptions, DmaPin, SyncMode,
};

/// A region of DMA memory, represented in virtual memory as type `T`, with a particular access mode
/// and options.
pub struct DmaRegion<'a, T: DeviceSync> {
    virt: *mut u8,
    backing: Option<(Vec<PhysInfo>, u32)>,
    len: usize,
    access: Access,
    dma: Option<&'a DmaObject>,
    pool: Option<(Arc<AllocatableDmaObject>, SplitPageRange)>,
    options: DmaOptions,
    offset: usize,
    _pd: PhantomData<T>,
}

/// A region of DMA memory, represented in virtual memory as type `[T; len]`, with a particular access mode
/// and options.
pub struct DmaSliceRegion<'a, T: DeviceSync> {
    region: DmaRegion<'a, T>,
    len: usize,
}

impl<'a, T: DeviceSync> DmaRegion<'a, T> {
    pub(super) fn new(
        dma: Option<&'a DmaObject>,
        len: usize,
        access: Access,
        options: DmaOptions,
        offset: usize,
        pool: Option<(Arc<AllocatableDmaObject>, SplitPageRange)>,
    ) -> Self {
        Self {
            virt: unsafe {
                dma.unwrap_or_else(|| pool.as_ref().unwrap().0.dma_object())
                    .object()
                    .base_mut_unchecked() as *mut () as *mut u8
            },
            len,
            access,
            dma,
            options,
            backing: None,
            pool,
            offset,
            _pd: PhantomData,
        }
    }

    pub(super) fn fill(&mut self, init: T) {
        let p = self.virt as *mut T;
        unsafe {
            p.write_volatile(init);
            self.sync(SyncMode::FullCoherence);
        }
    }

    fn dma_object(&self) -> &DmaObject {
        self.dma
            .unwrap_or_else(|| self.pool.as_ref().unwrap().0.dma_object())
    }

    /// Calculate the number of pages this region covers.
    pub fn nr_pages(&self) -> usize {
        (self.len - 1) / DMA_PAGE_SIZE + 1
    }

    fn setup_backing(&mut self) -> Result<(), PinError> {
        if self.backing.is_some() {
            return Ok(());
        }
        let mut pins = Vec::new();
        let len = self.nr_pages();
        pins.resize(len, PinnedPage::new(0));

        // The kaction call here compresses start and len into a u64, and returns the token and len
        // in the return u64. This is all because of the limited registers in a syscall.
        let start = (self.offset / DMA_PAGE_SIZE) as u64;
        let ptr = (&pins).as_ptr() as u64;
        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::PinPages(0)),
            Some(self.dma_object().object().id()),
            ptr,
            pack_kaction_pin_start_and_len(start, len).ok_or(PinError::InternalError)?,
            KactionFlags::empty(),
        )
        .map_err(|_| PinError::InternalError)?
        .u64()
        .ok_or(PinError::InternalError)?;

        let (token, retlen) =
            unpack_kaction_pin_token_and_len(res).ok_or(PinError::InternalError)?;

        if retlen < len {
            return Err(PinError::Exhausted);
        } else if retlen > len {
            return Err(PinError::InternalError);
        }

        let backing: Result<Vec<_>, _> = pins
            .iter()
            .map(|p| p.physical_address().try_into().map(|pa| PhysInfo::new(pa)))
            .collect();

        self.backing = Some((backing.map_err(|_| PinError::InternalError)?, token));

        Ok(())
    }

    /// Return the number of bytes this region covers.
    pub fn num_bytes(&self) -> usize {
        self.len
    }

    /// Return the access direction of this region.
    pub fn access(&self) -> Access {
        self.access
    }

    // Determines the backing information for region. This includes acquiring physical addresses for
    // the region and holding a pin for the pages.
    pub fn pin(&mut self) -> Result<DmaPin<'_>, PinError> {
        self.setup_backing()?;
        Ok(DmaPin::new(&self.backing.as_ref().unwrap().0))
    }

    // Synchronize the region for cache coherence.
    pub fn sync(&self, sync: SyncMode) {
        crate::arch::sync(self, sync, 0, self.len);
    }

    // Run a closure that takes a reference to the DMA data, ensuring coherence.
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        if !self.options.contains(DmaOptions::UNSAFE_MANUAL_COHERENCE) {
            if self.access() != Access::HostToDevice {
                self.sync(SyncMode::PostDeviceToCpu);
            }
        }
        let data = unsafe { self.get() };
        let ret = f(data);
        ret
    }

    // Run a closure that takes a mutable reference to the DMA data, ensuring coherence.
    pub fn with_mut<F, R>(&mut self, f: F) -> R
    where
        F: FnOnce(&mut T) -> R,
    {
        if !self.options.contains(DmaOptions::UNSAFE_MANUAL_COHERENCE) {
            match self.access() {
                Access::HostToDevice => self.sync(SyncMode::PreCpuToDevice),
                Access::DeviceToHost => self.sync(SyncMode::PostDeviceToCpu),
                Access::BiDirectional => self.sync(SyncMode::FullCoherence),
            }
        }
        let data = unsafe { self.get_mut() };
        let ret = f(data);
        if !self.options.contains(DmaOptions::UNSAFE_MANUAL_COHERENCE) {
            if self.access() != Access::DeviceToHost {
                self.sync(SyncMode::PostCpuToDevice);
            }
        }
        ret
    }

    /// Release any pin created for this region.
    ///
    /// # Safety
    /// Caller must ensure that no device is using the information from any active pins for this region.
    pub unsafe fn release_pin(&mut self) {
        if let Some((_, token)) = self.backing {
            super::object::release_pin(self.dma_object().object().id(), token);
            self.backing = None;
        }
    }

    /// Get a reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    #[inline]
    pub unsafe fn get(&self) -> &T {
        (self.virt as *const T).as_ref().unwrap()
    }

    /// Get a mutable reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    #[inline]
    pub unsafe fn get_mut(&mut self) -> &mut T {
        (self.virt as *mut T).as_mut().unwrap()
    }
}

impl<'a, T: DeviceSync> DmaSliceRegion<'a, T> {
    pub(super) fn new(
        dma: Option<&'a DmaObject>,
        nrbytes: usize,
        access: Access,
        options: DmaOptions,
        offset: usize,
        len: usize,
        pool: Option<(Arc<AllocatableDmaObject>, SplitPageRange)>,
    ) -> Self {
        Self {
            region: DmaRegion::new(dma, nrbytes, access, options, offset, pool),
            len,
        }
    }

    pub(super) fn fill(&mut self, init: T)
    where
        T: Clone,
    {
        let p = self.region.virt as *mut T;
        for idx in 0..self.len {
            unsafe {
                p.add(idx).write_volatile(init.clone());
            }
        }
        self.sync(0..self.len, SyncMode::FullCoherence);
    }

    pub(super) fn fill_with(&mut self, init: impl Fn() -> T) {
        let p = self.region.virt as *mut T;
        for idx in 0..self.len {
            unsafe {
                p.add(idx).write_volatile(init());
            }
        }
        self.sync(0..self.len, SyncMode::FullCoherence);
    }

    /// Return the number of bytes this region covers.
    pub fn num_bytes(&self) -> usize {
        self.region.len
    }

    #[inline]
    /// Return the access direction of this region.
    pub fn access(&self) -> Access {
        self.region.access()
    }

    /// Return the number of elements in the slice that this region covers.
    pub fn len(&self) -> usize {
        self.len
    }

    // Determines the backing information for region. This includes acquiring physical addresses for
    // the region and holding a pin for the pages.
    #[inline]
    pub fn pin(&mut self) -> Result<DmaPin<'_>, PinError> {
        self.region.pin()
    }

    // Synchronize a subslice of the region for cache coherence.
    pub fn sync(&self, range: Range<usize>, sync: SyncMode) {
        let start = range.start * core::mem::size_of::<T>();
        let len = range.len() * core::mem::size_of::<T>();
        crate::arch::sync(&self.region, sync, start, len);
    }

    // Run a closure that takes a reference to a subslice of the DMA data, ensuring coherence.
    pub fn with<F, R>(&self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&[T]) -> R,
    {
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            if self.access() != Access::HostToDevice {
                self.sync(range.clone(), SyncMode::PostDeviceToCpu);
            }
        }
        let data = &unsafe { self.get() }[range];
        let ret = f(data);
        ret
    }

    // Run a closure that takes a mutable reference to a subslice of the DMA data, ensuring coherence.
    pub fn with_mut<F, R>(&mut self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&mut [T]) -> R,
    {
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            match self.access() {
                Access::HostToDevice => self.sync(range.clone(), SyncMode::PreCpuToDevice),
                Access::DeviceToHost => self.sync(range.clone(), SyncMode::PostDeviceToCpu),
                Access::BiDirectional => self.sync(range.clone(), SyncMode::FullCoherence),
            }
        }
        let data = &mut unsafe { self.get_mut() }[range.clone()];
        let ret = f(data);
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            if self.access() != Access::DeviceToHost {
                self.sync(range, SyncMode::PostCpuToDevice);
            }
        }
        ret
    }

    /// Release any pin created for this region.
    ///
    /// # Safety
    /// Caller must ensure that no device is using the information from any active pins for this region.
    #[inline]
    pub unsafe fn release_pin(&mut self) {
        self.region.release_pin()
    }

    /// Get a reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    #[inline]
    pub unsafe fn get(&self) -> &[T] {
        core::slice::from_raw_parts(self.region.virt as *const T, self.len)
    }

    /// Get a mutable reference to the DMA memory.
    ///
    /// # Safety
    /// The caller must ensure coherence is applied.
    #[inline]
    pub unsafe fn get_mut(&mut self) -> &mut [T] {
        core::slice::from_raw_parts_mut(self.region.virt as *mut T, self.len)
    }
}

impl<'a, T: DeviceSync> Drop for DmaRegion<'a, T> {
    fn drop(&mut self) {
        if let Some((_, token)) = self.backing.as_ref() {
            self.dma_object()
                .releasable_pins
                .lock()
                .unwrap()
                .push(*token);
        }

        if let Some((ado, range)) = self.pool.take() {
            ado.free(range);
        }
    }
}
