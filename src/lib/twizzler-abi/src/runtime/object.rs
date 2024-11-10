//! Implementation of the object runtime.

use core::ptr::NonNull;

use super::MinimalRuntime;
use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::object::slot::global_allocate,
    rustc_alloc::boxed::Box,
    syscall::{sys_object_map, ObjectMapError, UnmapFlags},
};
use twizzler_rt_abi::object::{MapFlags, MapError, ObjectHandle};

mod handle;

#[allow(unused_imports)]
pub use handle::*;

pub(crate) mod slot;

impl From<MapFlags> for Protections {
    fn from(value: MapFlags) -> Self {
        let mut f = Self::empty();
        if value.contains(MapFlags::READ) {
            f.insert(Protections::READ);
        }
        if value.contains(MapFlags::WRITE) {
            f.insert(Protections::WRITE);
        }
        if value.contains(MapFlags::EXEC) {
            f.insert(Protections::EXEC);
        }
        f
    }
}

impl From<MapFlags> for crate::syscall::MapFlags {
    fn from(_value: MapFlags) -> Self {
        Self::empty()
    }
}

impl Into<MapError> for ObjectMapError {
    fn into(self) -> MapError {
        match self {
            ObjectMapError::Unknown => MapError::Other,
            ObjectMapError::ObjectNotFound => MapError::NoSuchObject,
            ObjectMapError::InvalidSlot => MapError::Other,
            ObjectMapError::InvalidProtections => MapError::PermissionDenied,
            ObjectMapError::InvalidArgument => MapError::InvalidArgument,
        }
    }
}

impl MinimalRuntime {
    pub fn map_object(
        &self,
        id: ObjID,
        flags: MapFlags,
    ) -> Result<ObjectHandle, MapError> {
        let slot = global_allocate().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(None, id, slot, flags.into(), flags.into()).map_err(|e| e.into())?;
        todo!()
        /*
        Ok(ObjectHandle::new(
            Some(NonNull::new(Box::into_raw(Box::new(InternalHandleRefs::default()))).unwrap()),
            id,
            flags,
            (slot * MAX_SIZE) as *mut u8,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        ))
        */
    }

    pub fn release_handle(&self, handle: &mut ObjectHandle) {
        let slot = (handle.start() as usize) / MAX_SIZE;

        if crate::syscall::sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            slot::global_release(slot);
        }
    }
}
