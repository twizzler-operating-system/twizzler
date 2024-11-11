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

    pub fn release_handle(&self, handle: *mut twizzler_rt_abi::bindings::object_handle) {
        let slot = (unsafe {(*handle).start} as usize) / MAX_SIZE;

        if crate::syscall::sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            slot::global_release(slot);
        }
    }

    
    /// Map two objects in sequence, useful for executable loading. The default implementation makes
    /// no guarantees about ordering.
    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle), MapError> {
        let map_and_check = |rev: bool| {
            let (id_a, flags_a) = if rev {
                (in_id_b, in_flags_b)
            } else {
                (in_id_a, in_flags_a)
            };

            let (id_b, flags_b) = if !rev {
                (in_id_b, in_flags_b)
            } else {
                (in_id_a, in_flags_a)
            };

            let a = self.map_object(id_a, flags_a)?;
            let b = self.map_object(id_b, flags_b)?;
            let a_addr = a.start() as usize;
            let b_addr = b.start() as usize;

            if rev && a_addr > b_addr {
                Ok((b, a))
            } else if !rev && b_addr > a_addr {
                Ok((a, b))
            } else {
                Err(MapError::Other)
            }
        };

        map_and_check(false).or_else(|_| map_and_check(true))
    }
}
