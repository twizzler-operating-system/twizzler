use std::marker::PhantomData;

pub use twizzler_abi::device::DeviceRepr;

pub struct Device {}

pub struct InfoObject<T> {
    _pd: PhantomData<T>,
}
pub struct MmioObject {}
pub struct DeviceChildrenIterator {}

impl Device {
    pub fn get_mmio(&self, _idx: usize) -> MmioObject {
        todo!()
    }

    pub unsafe fn get_info<T>(&self, _idx: usize) -> InfoObject<T> {
        todo!()
    }

    pub fn children(&self) -> DeviceChildrenIterator {
        todo!()
    }

    pub fn repr(&self) -> DeviceRepr {
        todo!()
    }
}

pub struct BusTreeRoot {}

impl BusTreeRoot {
    pub fn children() -> DeviceChildrenIterator {
        todo!()
    }
}

pub fn get_bustree_root() -> BusTreeRoot {
    todo!()
}
