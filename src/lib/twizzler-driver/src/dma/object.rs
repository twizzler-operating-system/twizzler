use std::sync::Mutex;

use twizzler::object::{ObjID, Object};
use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::sys_kaction,
};

use super::{Access, DeviceSync, DmaOptions, DmaRegion, DmaSliceRegion};

/// A handle for an object that can be used to perform DMA, and is most useful directly as a way to
/// perform DMA operations on a specific object. For an allocator-like DMA interface, see
/// [crate::dma::DmaPool].
pub struct DmaObject {
    obj: Object<()>,
    pub(crate) releasable_pins: Mutex<Vec<u32>>,
}

impl DmaObject {
    /// TODO: support
    /// Create a [DmaSliceRegion] from the base of this object, where the region represents memory
    /// of type `[T; len]`.
    fn _slice_region<T: DeviceSync>(
        &self,
        len: usize,
        access: Access,
        options: DmaOptions,
    ) -> DmaSliceRegion<T> {
        let nr_bytes = core::mem::size_of::<T>()
            .checked_mul(len)
            .expect("Value of len too large");
        assert!(nr_bytes < MAX_SIZE - NULLPAGE_SIZE * 2);
        DmaSliceRegion::new(
            core::mem::size_of::<T>() * len,
            access,
            options,
            NULLPAGE_SIZE,
            len,
            None,
        )
    }

    /// TODO: support
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

    /// Create a new [DmaObject] from an existing object handle.
    pub fn new<T>(obj: Object<T>) -> Self {
        Self {
            obj: unsafe { obj.cast() },
            releasable_pins: Mutex::new(Vec::new()),
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

impl Drop for DmaObject {
    fn drop(&mut self) {
        let pins = self.releasable_pins.lock().unwrap();
        for pin in &*pins {
            release_pin(self.object().id(), *pin);
        }
    }
}

impl<T> From<Object<T>> for DmaObject {
    fn from(obj: Object<T>) -> Self {
        Self::new(obj)
    }
}
