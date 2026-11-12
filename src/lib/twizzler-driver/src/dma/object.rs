use std::{
    ops::Range,
    sync::{Arc, Mutex},
};

use twizzler::{
    error::TwzError,
    object::{ObjID, Object, RawObject},
};
use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, PinExtent, PinFlags, PinRequest},
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::sys_kaction,
};

use super::{Access, DeviceSync, DmaOptions, DmaRegion, DmaSliceRegion};

/// A handle for an object that can be used to perform DMA, and is most useful directly as a way to
/// perform DMA operations on a specific object. For an allocator-like DMA interface, see
/// [crate::dma::DmaPool].
#[derive(Clone)]
pub struct DmaObject {
    obj: Object<()>,
    pub(crate) releasable_pins: Arc<Mutex<Vec<u32>>>,
}

impl DmaObject {
    /// Create a [DmaSliceRegion] from the base of this object, where the region represents memory
    /// of type `[T; len]`.
    pub fn slice_region<T: DeviceSync>(
        &self,
        offset: usize,
        len: usize,
        access: Access,
        options: DmaOptions,
    ) -> DmaSliceRegion<T> {
        let nr_bytes = core::mem::size_of::<T>()
            .checked_mul(len)
            .expect("Value of len too large");
        assert!(nr_bytes + offset < MAX_SIZE - NULLPAGE_SIZE * 2);
        let virt = self.obj.lea_mut(offset, nr_bytes).unwrap();
        DmaSliceRegion::new_with_virt(
            core::mem::size_of::<T>() * len,
            access,
            options,
            offset,
            len,
            virt,
            self.clone(),
        )
    }

    /// Create a [DmaRegion] from the base of this object, where the region represents memory
    /// of type `T`.
    fn _region<T: DeviceSync>(&self, access: Access, options: DmaOptions) -> DmaRegion<T> {
        DmaRegion::new(
            core::mem::size_of::<T>(),
            access,
            options,
            NULLPAGE_SIZE,
            None,
        )
    }

    /// Get a reference to the object handle.
    pub fn object(&self) -> &Object<()> {
        &self.obj
    }

    /// Pin the page-aligned byte range `range` of this object, populating it first, and get its
    /// physical extents in object order. Takes as many kernel calls as the extents need.
    pub fn pin_range(&self, range: Range<usize>, flags: PinFlags) -> Result<ObjectPin, TwzError> {
        let mut pin = ObjectPin {
            obj: self.clone(),
            extents: Vec::new(),
            tokens: Vec::new(),
        };
        let mut buf = [PinExtent::default(); 64];
        let mut at = range.start;
        while at < range.end {
            let mut req = PinRequest::new(at as u64, (range.end - at) as u64, &mut buf);
            if let Err(e) = sys_kaction(
                KactionCmd::Generic(KactionGenericCmd::PinRange(flags.bits())),
                Some(self.object().id()),
                &mut req as *mut PinRequest as u64,
                0,
                KactionFlags::empty(),
            ) {
                unsafe { pin.release() };
                return Err(e);
            }
            pin.tokens.push(req.token);
            self.releasable_pins.lock().unwrap().push(req.token);
            for ext in &buf[..req.nr as usize] {
                match pin.extents.last_mut() {
                    Some(last) if last.phys + last.len == ext.phys => last.len += ext.len,
                    _ => pin.extents.push(*ext),
                }
            }
            at += req.covered as usize;
        }
        Ok(pin)
    }

    /// Create a new [DmaObject] from an existing object handle.
    pub fn new<T>(obj: Object<T>) -> Self {
        Self {
            obj: unsafe { obj.cast() },
            releasable_pins: Arc::new(Mutex::new(Vec::new())),
        }
    }
}

pub(crate) fn release_pin(id: ObjID, token: u32) {
    let _ = sys_kaction(
        KactionCmd::Generic(KactionGenericCmd::ReleasePin),
        Some(id),
        token as u64,
        0,
        KactionFlags::empty(),
    );
}

/// A pinned range of a [DmaObject]. Not released on drop, because a device may still be using it;
/// dropping the last handle to the [DmaObject] releases it.
pub struct ObjectPin {
    obj: DmaObject,
    extents: Vec<PinExtent>,
    tokens: Vec<u32>,
}

impl ObjectPin {
    /// The physical runs backing the range, in object order.
    pub fn extents(&self) -> &[PinExtent] {
        &self.extents
    }

    /// Release the pin.
    ///
    /// # Safety
    /// No device may still be using the extents.
    pub unsafe fn release(self) {
        let mut pins = self.obj.releasable_pins.lock().unwrap();
        for token in &self.tokens {
            release_pin(self.obj.object().id(), *token);
            if let Some(pos) = pins.iter().position(|t| t == token) {
                pins.swap_remove(pos);
            }
        }
    }
}

impl Drop for DmaObject {
    fn drop(&mut self) {
        let mut pins = self.releasable_pins.lock().unwrap();
        for pin in pins.drain(..) {
            release_pin(self.object().id(), pin);
        }
    }
}

impl<T> From<Object<T>> for DmaObject {
    fn from(obj: Object<T>) -> Self {
        Self::new(obj)
    }
}
