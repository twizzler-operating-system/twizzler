use twizzler_abi::device::{MmioInfo, MMIO_OFFSET, SubObjectType};
use twizzler_object::{ObjectInitError, Object, Protections, ObjectInitFlags, ObjID};

use super::Device;

pub struct MmioObject {
    obj: Object<MmioInfo>,
}

impl MmioObject {
    pub(crate) fn new(id: ObjID) -> Result<Self, ObjectInitError> {
        Ok(Self {
            obj: Object::init_id(
                id,
                Protections::READ | Protections::WRITE,
                ObjectInitFlags::empty(),
            )?,
        })
    }

    // TODO: no unwrap
    pub fn get_info(&self) -> &MmioInfo {
        self.obj.base().unwrap()
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset<T>(&self, offset: usize) -> &T {
        let ptr = self.obj.base().unwrap() as *const MmioInfo as *const u8;
        (ptr.add(MMIO_OFFSET + offset).sub(0x1000) as *mut T)
            .as_mut()
            .unwrap()
    }
}

impl Device {
    pub fn get_mmio(&self, idx: u8) -> Option<MmioObject> {
        let id = self.get_subobj(SubObjectType::Mmio.into(), idx)?;
        MmioObject::new(id).ok()
    }
}
