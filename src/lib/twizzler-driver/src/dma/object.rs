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
        todo!()
    }

    pub fn region<'a, T: DeviceSync>(
        &'a self,
        access: Access,
        options: DmaOptions,
    ) -> DmaRegion<'a, T> {
        todo!()
    }

    pub fn object(&self) -> &Object<()> {
        todo!()
    }

    pub fn new<T>(&self, obj: Object<T>) -> Self {
        todo!()
    }
}

impl Drop for DmaObject {
    fn drop(&mut self) {
        todo!()
    }
}

impl<T> From<Object<T>> for DmaObject {
    fn from(obj: Object<T>) -> Self {
        todo!()
    }
}
