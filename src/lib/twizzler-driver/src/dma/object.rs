use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_object::Object;

use super::{Access, DeviceSync, DmaArrayRegion, DmaOptions, DmaRegion};

pub struct DmaObject {
    obj: Object<()>,
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
        todo!()
    }

    pub fn new<T>(obj: Object<T>) -> Self {
        Self {
            obj: unsafe { obj.transmute() },
        }
    }
}

impl Drop for DmaObject {
    fn drop(&mut self) {
        todo!()
    }
}

impl<T> From<Object<T>> for DmaObject {
    fn from(obj: Object<T>) -> Self {
        Self::new(obj)
    }
}
