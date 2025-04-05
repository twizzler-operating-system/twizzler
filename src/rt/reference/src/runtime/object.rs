use std::{ffi::c_void, sync::atomic::AtomicU64};

use handlecache::HandleCache;
use tracing::warn;
use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::{
    bindings::object_handle,
    error::{ArgumentError, TwzError},
    object::{MapFlags, ObjID, ObjectHandle},
    Result,
};

use super::ReferenceRuntime;

mod handlecache;

#[repr(C)]
pub(crate) struct RuntimeHandleInfo {
    refs: AtomicU64,
}

pub(crate) fn new_runtime_info() -> *mut RuntimeHandleInfo {
    let rhi = Box::new(RuntimeHandleInfo {
        refs: AtomicU64::new(1),
    });
    Box::into_raw(rhi)
}

pub(crate) fn free_runtime_info(ptr: *mut RuntimeHandleInfo) {
    if ptr.is_null() {
        return;
    }
    let _boxed = unsafe { Box::from_raw(ptr) };
}

pub(crate) fn new_object_handle(id: ObjID, slot: usize, flags: MapFlags) -> ObjectHandle {
    unsafe {
        ObjectHandle::new(
            id,
            new_runtime_info().cast(),
            (slot * MAX_SIZE) as *mut _,
            (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut _,
            flags,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }
}

impl ReferenceRuntime {
    #[tracing::instrument(ret, skip(self), level = "trace")]
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle> {
        self.object_manager
            .lock()
            .map_object(ObjectMapKey(id.into(), flags))
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn release_handle(&self, handle: *mut object_handle) {
        self.object_manager.lock().release(handle);
        if self.is_monitor().is_some() {
            self.object_manager.lock().cache.flush();
        }
    }

    pub fn get_object_handle_from_ptr(&self, ptr: *const u8) -> Result<object_handle> {
        if let Some(handle) = self.object_manager.lock().get_handle(ptr) {
            return Ok(handle);
        }

        let id = self
            .get_alloc()
            .get_id_from_ptr(ptr)
            .ok_or(ArgumentError::InvalidAddress)?;
        let slot = ptr as usize / MAX_SIZE;
        Ok(object_handle {
            id: id.raw(),
            start: (slot * MAX_SIZE) as *mut c_void,
            map_flags: (MapFlags::READ | MapFlags::WRITE).bits(),
            ..Default::default()
        })
    }

    pub fn insert_fot(&self, _handle: *mut object_handle, _fot: *const u8) -> Result<u32> {
        tracing::warn!("TODO: insert FOT entry");
        Err(TwzError::NOT_SUPPORTED)
    }

    pub fn resolve_fot(
        &self,
        _handle: *mut object_handle,
        _idx: u64,
        _valid_len: usize,
    ) -> Result<ObjectHandle> {
        tracing::warn!("TODO: resolve FOT entry");
        Err(TwzError::NOT_SUPPORTED)
    }

    pub fn resolve_fot_local(&self, _ptr: *mut u8, _idx: u64, _valid_len: usize) -> *mut u8 {
        tracing::warn!("TODO: resolve local FOT entry");
        core::ptr::null_mut()
    }

    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle)> {
        let mapping =
            monitor_api::monitor_rt_object_pair_map(in_id_a, in_flags_a, in_id_b, in_flags_b)?;

        let handle = new_object_handle(in_id_a, mapping.0.slot, in_flags_a);
        let handle2 = new_object_handle(in_id_b, mapping.1.slot, in_flags_b);
        Ok((handle, handle2))
    }
}

/// A key for local (per-compartment) mappings of objects.
#[derive(PartialEq, PartialOrd, Ord, Eq, Hash, Copy, Clone, Debug)]
pub struct ObjectMapKey(pub ObjID, pub MapFlags);

impl ObjectMapKey {
    pub fn from_raw_handle(handle: &object_handle) -> Self {
        Self(
            handle.id.into(),
            MapFlags::from_bits_truncate(handle.map_flags),
        )
    }
}

/// Per-compartment object management.
pub struct ObjectHandleManager {
    cache: HandleCache,
}

impl ObjectHandleManager {
    pub const fn new() -> Self {
        Self {
            cache: HandleCache::new(),
        }
    }

    /// Map an object with this manager. Will call to monitor if needed.
    pub fn map_object(&mut self, key: ObjectMapKey) -> Result<ObjectHandle> {
        if let Some(handle) = self.cache.activate(key) {
            let oh = ObjectHandle::from_raw(handle);
            let oh2 = oh.clone();
            std::mem::forget(oh);
            return Ok(oh2);
        }
        let mapping = monitor_api::monitor_rt_object_map(key.0, key.1)?;
        let handle = new_object_handle(key.0, mapping.slot, key.1).into_raw();
        self.cache.insert(handle);
        Ok(ObjectHandle::from_raw(handle))
    }

    /// Get an object handle from a pointer to within that object.
    pub fn get_handle(&mut self, ptr: *const u8) -> Option<object_handle> {
        let handle = self.cache.activate_from_ptr(ptr)?;
        let oh = ObjectHandle::from_raw(handle);
        let oh2 = oh.clone().into_raw();
        std::mem::forget(oh);
        Some(oh2)
    }

    /// Release a handle. If all handles have been released, calls to monitor to unmap.
    pub fn release(&mut self, handle: *mut object_handle) {
        let handle = unsafe { handle.as_mut().unwrap() };
        self.cache.release(handle);
    }
}
