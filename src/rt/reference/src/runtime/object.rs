use std::{ffi::c_void, mem::ManuallyDrop, sync::atomic::AtomicU64, usize::MAX};

use handlecache::HandleCache;
use tracing::warn;
use twizzler_abi::{
    object::{Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, ObjectMapError},
};
use twizzler_rt_abi::{
    bindings::object_handle,
    object::{MapError, MapFlags, ObjID, ObjectHandle},
};

use super::ReferenceRuntime;

mod handlecache;

fn mapflags_into_prot(flags: MapFlags) -> Protections {
    let mut prot = Protections::empty();
    if flags.contains(MapFlags::READ) {
        prot.insert(Protections::READ);
    }
    if flags.contains(MapFlags::WRITE) {
        prot.insert(Protections::WRITE);
    }
    if flags.contains(MapFlags::EXEC) {
        prot.insert(Protections::EXEC);
    }
    prot
}

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

fn map_sys_err(sys_err: ObjectMapError) -> MapError {
    // TODO (dbittman): in a future PR, I plan to cleanup all the error handling between the API and
    // ABI crates.
    match sys_err {
        ObjectMapError::Unknown => MapError::Other,
        ObjectMapError::ObjectNotFound => MapError::NoSuchObject,
        ObjectMapError::InvalidSlot => MapError::InvalidArgument,
        ObjectMapError::InvalidProtections => MapError::PermissionDenied,
        ObjectMapError::InvalidArgument => MapError::InvalidArgument,
    }
}

impl ReferenceRuntime {
    #[tracing::instrument(ret, skip(self), level = "trace")]
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError> {
        self.object_manager
            .lock()
            .map_object(ObjectMapKey(id.into(), flags))
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn release_handle(&self, handle: *mut object_handle) {
        self.object_manager.lock().release(handle)
    }

    pub fn get_object_handle_from_ptr(&self, ptr: *const u8) -> Option<object_handle> {
        if let Some(handle) = self.object_manager.lock().get_handle(ptr) {
            return Some(handle);
        }

        let id = self.get_alloc().get_id_from_ptr(ptr)?;
        let slot = ptr as usize / MAX_SIZE;
        Some(object_handle {
            id: id.raw(),
            start: (slot * MAX_SIZE) as *mut c_void,
            map_flags: (MapFlags::READ | MapFlags::WRITE).bits(),
            ..Default::default()
        })
    }

    pub fn insert_fot(&self, handle: *mut object_handle, fot: *const u8) -> Option<u64> {
        tracing::warn!("TODO: insert FOT entry");
        None
    }

    pub fn resolve_fot(
        &self,
        handle: *mut object_handle,
        idx: u64,
        valid_len: usize,
    ) -> Result<ObjectHandle, MapError> {
        tracing::warn!("TODO: resolve FOT entry");
        Err(MapError::Other)
    }

    pub fn resolve_fot_local(&self, ptr: *mut u8, idx: u64, valid_len: usize) -> *mut u8 {
        tracing::warn!("TODO: resolve local FOT entry");
        core::ptr::null_mut()
    }

    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle), MapError> {
        let (slot_a, slot_b) = self.allocate_pair().ok_or(MapError::OutOfResources)?;

        let prot_a = mapflags_into_prot(in_flags_a);
        let prot_b = mapflags_into_prot(in_flags_b);

        sys_object_map(
            None,
            in_id_a,
            slot_a,
            prot_a,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(map_sys_err)?;

        sys_object_map(
            None,
            in_id_b,
            slot_b,
            prot_b,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .map_err(map_sys_err)?;

        Ok((
            new_object_handle(in_id_a, slot_a, in_flags_a),
            new_object_handle(in_id_b, slot_b, in_flags_b),
        ))
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
    pub fn map_object(&mut self, key: ObjectMapKey) -> Result<ObjectHandle, MapError> {
        if let Some(handle) = self.cache.activate(key) {
            let oh = ObjectHandle::from_raw(handle);
            let oh2 = oh.clone();
            std::mem::forget(oh);
            return Ok(oh2);
        }
        let mapping = monitor_api::monitor_rt_object_map(key.0, key.1).unwrap()?;
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
