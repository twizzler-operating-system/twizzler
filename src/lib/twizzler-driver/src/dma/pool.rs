use twizzler_object::CreateSpec;

use super::{Access, DeviceSync, DmaOptions, DmaRegion, DmaSliceRegion};

pub struct DmaPool {
    opts: DmaOptions,
}

impl DmaPool {
    pub fn new(spec: CreateSpec, access: Access, opts: DmaOptions) -> Self {
        todo!()
    }

    pub fn default_spec() -> CreateSpec {
        todo!()
    }

    // TODO: update so these are failable
    pub fn allocate<'a, T: DeviceSync>(&'a self, init: T) -> DmaRegion<'a, T> {
        todo!()
    }

    pub fn allocate_with<'a, T: DeviceSync>(&'a self, init: impl Fn() -> T) -> DmaRegion<'a, T> {
        todo!()
    }

    pub fn allocate_array<'a, T: DeviceSync>(&'a self, init: T) -> DmaSliceRegion<'a, T> {
        todo!()
    }

    pub fn allocate_array_with<'a, T: DeviceSync>(
        &'a self,
        init: impl Fn() -> T,
    ) -> DmaSliceRegion<'a, T> {
        todo!()
    }
}

impl Drop for DmaPool {
    fn drop(&mut self) {
        todo!()
    }
}
