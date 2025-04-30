use twizzler::object::{ObjID, Object, RawObject};
use twizzler_abi::device::SubObjectType;
use twizzler_rt_abi::{object::MapFlags, Result};

use super::Device;

/// A handle to an info subobject.
pub struct InfoObject<T> {
    obj: Object<T>,
}

impl<T> InfoObject<T> {
    fn new(id: ObjID) -> Result<Self> {
        Ok(Self {
            obj: unsafe { Object::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?,
        })
    }

    /// Get a reference to the data contained within an info type subobject.
    pub fn get_data(&self) -> &T {
        unsafe { self.obj.base_ptr::<T>().as_ref().unwrap() }
    }
}

impl Device {
    /// Get an indexed info object for a device.
    /// # Safety
    /// The type T is not verified in any way, so the caller must ensure that T is correct
    /// for the underlying data.
    pub unsafe fn get_info<T>(&self, idx: u8) -> Option<InfoObject<T>> {
        let id = self.get_subobj(SubObjectType::Info.into(), idx)?;
        InfoObject::new(id).ok()
    }
}
