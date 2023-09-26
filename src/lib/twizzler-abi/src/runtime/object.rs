//! Implementation of the object runtime.

use twizzler_runtime_api::{MapError, ObjectHandle, ObjectRuntime};

use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::object::slot::global_allocate,
    syscall::{sys_object_map, ObjectMapError},
};

use super::MinimalRuntime;

mod handle;

#[allow(unused_imports)]
pub use handle::*;

pub(crate) mod slot;

impl From<twizzler_runtime_api::MapFlags> for Protections {
    fn from(value: twizzler_runtime_api::MapFlags) -> Self {
        let mut f = Self::empty();
        if value.contains(twizzler_runtime_api::MapFlags::READ) {
            f.insert(Protections::READ);
        }
        if value.contains(twizzler_runtime_api::MapFlags::WRITE) {
            f.insert(Protections::WRITE);
        }
        if value.contains(twizzler_runtime_api::MapFlags::EXEC) {
            f.insert(Protections::EXEC);
        }
        f
    }
}

impl From<twizzler_runtime_api::MapFlags> for crate::syscall::MapFlags {
    fn from(_value: twizzler_runtime_api::MapFlags) -> Self {
        Self::empty()
    }
}

impl Into<twizzler_runtime_api::MapError> for ObjectMapError {
    fn into(self) -> twizzler_runtime_api::MapError {
        match self {
            ObjectMapError::Unknown => twizzler_runtime_api::MapError::Other,
            ObjectMapError::ObjectNotFound => twizzler_runtime_api::MapError::NoSuchObject,
            ObjectMapError::InvalidSlot => twizzler_runtime_api::MapError::InternalError,
            ObjectMapError::InvalidProtections => twizzler_runtime_api::MapError::PermissionDenied,
            ObjectMapError::InvalidArgument => twizzler_runtime_api::MapError::InvalidArgument,
        }
    }
}

impl ObjectRuntime for MinimalRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        let slot = global_allocate().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(None, ObjID::new(id), slot, flags.into(), flags.into())
            .map_err(|e| e.into())?;
        Ok(ObjectHandle {
            id,
            flags,
            start: (slot * MAX_SIZE) as *mut u8,
            meta: (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        })
    }

    fn unmap_object(&self, _handle: &twizzler_runtime_api::ObjectHandle) {}

    fn release_handle(&self, _handle: &mut twizzler_runtime_api::ObjectHandle) {}
}
