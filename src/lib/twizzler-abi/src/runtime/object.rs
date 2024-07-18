//! Implementation of the object runtime.

use core::{mem::ManuallyDrop, ptr::NonNull};

use rustc_alloc::collections::BTreeMap;
use twizzler_runtime_api::{InternalHandleRefs, MapError, ObjectHandle, ObjectRuntime};

use super::{simple_mutex, MinimalRuntime};
use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::object::slot::global_allocate,
    rustc_alloc::boxed::Box,
    syscall::{sys_object_map, ObjectMapError, UnmapFlags},
};

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

static HANDLE_MAP: simple_mutex::Mutex<BTreeMap<usize, ManuallyDrop<ObjectHandle>>> =
    simple_mutex::Mutex::new(BTreeMap::new());

impl ObjectRuntime for MinimalRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        let slot = global_allocate().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(None, id, slot, flags.into(), flags.into()).map_err(|e| e.into())?;

        let refs = NonNull::new(Box::into_raw(Box::new(InternalHandleRefs::default()))).unwrap();
        let handle = ObjectHandle::new(
            refs,
            id,
            flags,
            (slot * MAX_SIZE) as *mut u8,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        );
        // We COPY the refs here, because our entry in the handle map does not hold a counted
        // reference to the handle, hence the manually-drop semantics.
        let our_handle = ManuallyDrop::new(ObjectHandle::new(
            refs,
            id,
            flags,
            (slot * MAX_SIZE) as *mut u8,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
        ));
        HANDLE_MAP.lock().insert(handle.start as usize, our_handle);

        Ok(handle)
    }

    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        let slot = (handle.start as usize) / MAX_SIZE;

        // This does not run drop on the handle, which is important, since we this map does not hold
        // a counted reference.
        if let Some(item) = HANDLE_MAP.lock().remove(&(handle.start as usize)) {
            // No one else has a reference outside of the runtime, since we're in release, and we've
            // just removed the last reference in the handle map. We can free the internal refs.
            unsafe {
                drop(Box::from_raw(item.internal_refs.as_ptr()));
            }
        }

        if crate::syscall::sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            slot::global_release(slot);
        }
    }

    fn ptr_to_handle(&self, va: *const u8) -> Option<ObjectHandle> {
        let start = self.ptr_to_object_start(va, 0)?;
        let hmap = HANDLE_MAP.lock();
        let our_handle = hmap.get(&(start as usize))?;

        // Clone will kick up the refcount again.
        let handle = ManuallyDrop::into_inner(our_handle.clone());
        Some(handle)
    }

    fn ptr_to_object_start(&self, va: *const u8, valid_len: usize) -> Option<*const u8> {
        let slot = (va as usize) / MAX_SIZE;
        Some((slot * MAX_SIZE) as *const u8)
    }

    fn resolve_fot_to_object_start<'a>(
        &self,
        handle: &'a ObjectHandle,
        idx: usize,
        valid_len: usize,
    ) -> Result<*const u8, twizzler_runtime_api::FotResolveError> {
        todo!()
    }

    fn add_fot_entry(&self, handle: &ObjectHandle) -> Option<(*mut u8, usize)> {
        todo!()
    }
}
