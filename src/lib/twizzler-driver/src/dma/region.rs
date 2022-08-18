use core::marker::PhantomData;
use core::ops::Range;

use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
    syscall::{sys_kaction, PinnedPage},
};

use super::{
    pin::{PhysInfo, PinError},
    Access, DeviceSync, DmaObject, DmaOptions, DmaPin, SyncMode,
};

pub struct DmaRegion<'a, T: DeviceSync> {
    virt: *mut u8,
    backing: Option<(Vec<PhysInfo>, u32)>,
    len: usize,
    access: Access,
    dma: &'a DmaObject,
    options: DmaOptions,
    offset: usize,
    _pd: PhantomData<T>,
}

pub struct DmaArrayRegion<'a, T: DeviceSync> {
    region: DmaRegion<'a, T>,
    len: usize,
}

impl<'a, T: DeviceSync> DmaRegion<'a, T> {
    pub(super) fn new(
        dma: &'a DmaObject,
        len: usize,
        access: Access,
        options: DmaOptions,
        offset: usize,
    ) -> Self {
        Self {
            virt: unsafe { dma.object().base_mut_unchecked() as *mut () as *mut u8 },
            len,
            access,
            dma,
            options,
            backing: None,
            offset,
            _pd: PhantomData,
        }
    }

    pub fn nr_pages(&self) -> usize {
        // TODO: arch-dep
        (self.len - 1) / 0x1000 + 1
    }

    fn setup_backing(&mut self) -> Result<(), PinError> {
        if self.backing.is_some() {
            return Ok(());
        }
        let mut pins = Vec::new();
        let len = self.nr_pages();
        pins.resize(len, PinnedPage::new(0));

        // TODO: arch-dep
        let start = self.offset as u64 / 0x1000;

        let ptr = (&pins).as_ptr() as u64;

        let res = sys_kaction(
            KactionCmd::Generic(KactionGenericCmd::PinPages(0)),
            Some(self.dma.object().id()),
            ptr,
            start | ((len as u64) << 32),
            KactionFlags::empty(),
        )
        .map_err(|_| PinError::InternalError)?
        .u64()
        .ok_or(PinError::InternalError)?;

        let retlen = (res >> 32) as usize;
        let token = (res & 0xffffffff) as u32;

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

    pub fn num_bytes(&self) -> usize {
        self.len
    }

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
    pub fn sync(&self, _sync: SyncMode) {
        crate::arch::sync(self);
        todo!()
    }

    // Run a closure that takes a reference to the DMA data, ensuring coherence.
    pub fn with<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&T) -> R,
    {
        if !self.options.contains(DmaOptions::UNSAFE_MANUAL_COHERENCE) {
            self.sync(SyncMode::PostDeviceToCpu);
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
            // TODO: combine these (update RFC)
            self.sync(SyncMode::PostDeviceToCpu);
            self.sync(SyncMode::PreCpuToDevice);
        }
        let data = unsafe { self.get_mut() };
        let ret = f(data);
        if !self.options.contains(DmaOptions::UNSAFE_MANUAL_COHERENCE) {
            self.sync(SyncMode::PostCpuToDevice);
        }
        ret
    }

    /// Release any pin created for this region.
    ///
    /// # Safety
    /// Caller must ensure that no device is using the information from any active pins for this region.
    pub unsafe fn release_pin(&mut self) {
        if let Some((_, token)) = self.backing {
            super::object::release_pin(self.dma.object().id(), token);
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

impl<'a, T: DeviceSync> DmaArrayRegion<'a, T> {
    pub(super) fn new(
        dma: &'a DmaObject,
        nrbytes: usize,
        access: Access,
        options: DmaOptions,
        offset: usize,
        len: usize,
    ) -> Self {
        Self {
            region: DmaRegion::new(dma, nrbytes, access, options, offset),
            len,
        }
    }

    pub fn num_bytes(&self) -> usize {
        self.region.len
    }

    #[inline]
    pub fn access(&self) -> Access {
        self.region.access()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    // TODO: update RFC
    // Determines the backing information for region. This includes acquiring physical addresses for
    // the region and holding a pin for the pages.
    #[inline]
    pub fn pin(&mut self) -> Result<DmaPin<'_>, PinError> {
        self.region.pin()
    }

    // Synchronize the region for cache coherence.
    pub fn sync(&self, _range: Range<usize>, sync: SyncMode) {
        // TODO: sync subset
        self.region.sync(sync)
    }

    // Run a closure that takes a reference to the DMA data, ensuring coherence.
    pub fn with<F, R>(&self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&[T]) -> R,
    {
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            self.sync(range.clone(), SyncMode::PostDeviceToCpu);
        }
        let data = &unsafe { self.get() }[range];
        let ret = f(data);
        ret
    }

    // Run a closure that takes a mutable reference to the DMA data, ensuring coherence.
    pub fn with_mut<F, R>(&mut self, range: Range<usize>, f: F) -> R
    where
        F: FnOnce(&mut [T]) -> R,
    {
        // TODO: combine these (update RFC)
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            self.sync(range.clone(), SyncMode::PostDeviceToCpu);
            self.sync(range.clone(), SyncMode::PreCpuToDevice);
        }
        let data = &mut unsafe { self.get_mut() }[range.clone()];
        let ret = f(data);
        if !self
            .region
            .options
            .contains(DmaOptions::UNSAFE_MANUAL_COHERENCE)
        {
            self.sync(range, SyncMode::PostCpuToDevice);
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
            self.dma.releasable_pins.lock().unwrap().push(*token);
        }
    }
}
