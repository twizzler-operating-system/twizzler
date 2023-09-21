use twizzler_abi::device::{MmioInfo, SubObjectType, MMIO_OFFSET};
use twizzler_object::{ObjID, Object, ObjectInitError, ObjectInitFlags, Protections};
use volatile::access::{ReadOnly, ReadWrite};

use super::Device;

/// A handle to an MMIO subobject.
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

    /// Get a reference to an MMIO subobject's info data.
    pub fn get_info(&self) -> &MmioInfo {
        self.obj.base().unwrap()
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset<T>(
        &self,
        offset: usize,
    ) -> volatile::VolatilePtr<'_, T, ReadOnly> {
        let ptr = self.obj.base().unwrap() as *const MmioInfo as *const u8;
        volatile::VolatileRef::from_ref(
            (ptr.add(MMIO_OFFSET + offset).sub(0x1000) as *mut T)
                .as_mut()
                .unwrap(),
        )
        .as_ptr()
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset_mut<T>(
        &self,
        offset: usize,
    ) -> volatile::VolatilePtr<'_, T, ReadWrite> {
        let ptr = self.obj.base().unwrap() as *const MmioInfo as *const u8;
        volatile::VolatileRef::from_mut_ref(
            (ptr.add(MMIO_OFFSET + offset).sub(0x1000) as *mut T)
                .as_mut()
                .unwrap(),
        )
        .as_mut_ptr()
    }
}

impl Device {
    /// Get a handle to a MMIO type subobject.
    pub fn get_mmio(&self, idx: u8) -> Option<MmioObject> {
        let id = self.get_subobj(SubObjectType::Mmio.into(), idx)?;
        MmioObject::new(id).ok()
    }
}
