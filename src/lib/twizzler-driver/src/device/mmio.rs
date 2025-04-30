use twizzler::object::{ObjID, Object, RawObject};
use twizzler_abi::{
    device::{MmioInfo, SubObjectType, MMIO_OFFSET},
    object::NULLPAGE_SIZE,
};
use twizzler_rt_abi::{object::MapFlags, Result};
use volatile::access::{ReadOnly, ReadWrite};

use super::Device;

/// A handle to an MMIO subobject.
pub struct MmioObject {
    obj: Object<MmioInfo>,
}

impl MmioObject {
    pub(crate) fn new(id: ObjID) -> Result<Self> {
        Ok(Self {
            obj: unsafe { Object::map_unchecked(id, MapFlags::READ | MapFlags::WRITE) }?,
        })
    }

    /// Get a reference to an MMIO subobject's info data.
    pub fn get_info(&self) -> &MmioInfo {
        unsafe { self.obj.base_ptr::<MmioInfo>().as_ref().unwrap() }
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset<T>(
        &self,
        offset: usize,
    ) -> volatile::VolatileRef<'_, T, ReadOnly> {
        let ptr = self.obj.base_ptr::<u8>();
        volatile::VolatileRef::from_ref(
            (ptr.add(MMIO_OFFSET + offset).sub(NULLPAGE_SIZE) as *mut T)
                .as_mut()
                .unwrap(),
        )
    }

    /// Get the base of the memory mapped IO region.
    /// # Safety
    /// The type this returns is not verified in any way, so the caller must ensure that T is
    /// the correct type for the underlying data.
    pub unsafe fn get_mmio_offset_mut<T>(
        &self,
        offset: usize,
    ) -> volatile::VolatileRef<'_, T, ReadWrite> {
        let ptr = self.obj.base_ptr::<u8>();
        volatile::VolatileRef::from_mut_ref(
            (ptr.add(MMIO_OFFSET + offset).sub(NULLPAGE_SIZE) as *mut T)
                .as_mut()
                .unwrap(),
        )
    }
}

impl Device {
    /// Get a handle to a MMIO type subobject.
    pub fn get_mmio(&self, idx: u8) -> Option<MmioObject> {
        let id = self.get_subobj(SubObjectType::Mmio.into(), idx)?;
        MmioObject::new(id).ok()
    }
}
