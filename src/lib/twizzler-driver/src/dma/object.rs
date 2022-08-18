use std::sync::Mutex;

use twizzler_abi::{
    kso::{KactionCmd, KactionFlags, KactionGenericCmd},
    object::NULLPAGE_SIZE,
    syscall::sys_kaction,
};
use twizzler_object::Object;

use super::{Access, DeviceSync, DmaArrayRegion, DmaOptions, DmaRegion};

pub struct DmaObject {
    obj: Object<()>,
    pub(crate) releasable_pins: Mutex<Vec<u32>>,
}

impl DmaObject {
    pub fn slice_region<'a, T: DeviceSync>(
        &'a self,
        len: usize,
        access: Access,
        options: DmaOptions,
    ) -> DmaArrayRegion<'a, T> {
        DmaArrayRegion::new(
            self,
            core::mem::size_of::<T>() * len,
            access,
            options,
            NULLPAGE_SIZE,
            len,
        )
    }

    pub fn region<'a, T: DeviceSync>(
        &'a self,
        access: Access,
        options: DmaOptions,
    ) -> DmaRegion<'a, T> {
        DmaRegion::new(
            self,
            core::mem::size_of::<T>(),
            access,
            options,
            NULLPAGE_SIZE,
        )
    }

    pub fn object(&self) -> &Object<()> {
        &self.obj
    }

    pub fn new<T>(obj: Object<T>) -> Self {
        Self {
            obj: unsafe { obj.transmute() },
            releasable_pins: Mutex::new(Vec::new()),
        }
    }
}

impl Drop for DmaObject {
    fn drop(&mut self) {
        let pins = self.releasable_pins.lock().unwrap();
        for pin in &*pins {
            let _ = sys_kaction(
                KactionCmd::Generic(KactionGenericCmd::ReleasePin),
                Some(self.object().id()),
                *pin as u64,
                0,
                KactionFlags::empty(),
            );
        }
    }
}

impl<T> From<Object<T>> for DmaObject {
    fn from(obj: Object<T>) -> Self {
        Self::new(obj)
    }
}
