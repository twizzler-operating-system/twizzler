use twizzler_abi::device::SubObjectType;
use twizzler_object::{ObjID, Object, ObjectInitError, Protections, ObjectInitFlags};

use super::Device;

pub struct InfoObject<T> {
    obj: Object<T>,
}

impl<T> InfoObject<T> {
    fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            obj: Object::init_id(id, Protections::READ, ObjectInitFlags::empty())?,
        })
    }

    pub fn get_data(&self) -> &T {
        unsafe { self.obj.base_unchecked() }
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
