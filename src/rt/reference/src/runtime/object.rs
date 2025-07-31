use std::{
    ffi::c_void,
    sync::atomic::{AtomicU64, Ordering},
};

use fotcache::FotCache;
use handlecache::HandleCache;
use tracing::warn;
use twizzler_abi::{
    meta::{FotEntry, FotFlags},
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_map_ctrl, sys_object_create, sys_object_read_map, CreateTieFlags, CreateTieSpec,
        MapControlCmd, ObjectCreate,
    },
};
use twizzler_rt_abi::{
    bindings::object_handle,
    error::{ObjectError, ResourceError, TwzError},
    object::{MapFlags, ObjID, ObjectHandle},
    Result,
};

use super::ReferenceRuntime;

mod fotcache;
mod handlecache;

#[repr(C)]
pub(crate) struct RuntimeHandleInfo {
    refs: AtomicU64,
    fot_cache: FotCache,
}

pub(crate) fn new_runtime_info() -> *mut RuntimeHandleInfo {
    let rhi = Box::new(RuntimeHandleInfo {
        refs: AtomicU64::new(1),
        fot_cache: FotCache::new(),
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

    pub fn create_rtobj(&self) -> Result<ObjID> {
        let tie_id = monitor_api::get_comp_config().sctx;
        sys_object_create(
            ObjectCreate::default(),
            &[],
            &[CreateTieSpec::new(tie_id, CreateTieFlags::empty())],
        )
    }

    pub fn update_handle(&self, handle: *mut object_handle) -> Result<()> {
        sys_map_ctrl(
            unsafe { &*handle }.start.cast(),
            MAX_SIZE,
            MapControlCmd::Update,
            0,
        )?;
        unsafe { &*(&*handle).runtime_info.cast::<RuntimeHandleInfo>() }
            .fot_cache
            .clear();
        Ok(())
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

        let slot = ptr as usize / MAX_SIZE;
        let Some(id) = self.get_alloc().get_id_from_ptr(ptr) else {
            let map = sys_object_read_map(None, slot)?;
            return Ok(object_handle {
                id: map.id.raw(),
                start: (slot * MAX_SIZE) as *mut c_void,
                map_flags: map.flags.bits(),
                ..Default::default()
            });
        };
        Ok(object_handle {
            id: id.raw(),
            start: (slot * MAX_SIZE) as *mut c_void,
            map_flags: (MapFlags::READ | MapFlags::WRITE).bits(),
            ..Default::default()
        })
    }

    pub fn insert_fot(&self, handle: *mut object_handle, fot: *const u8) -> Result<u32> {
        //tracing::warn!("TODO: insert FOT entry");
        let handle = unsafe { &*handle };
        // TODO: track max FOT entry
        let _meta = unsafe { &*handle.meta };
        let new_fot = unsafe { fot.cast::<FotEntry>().read() };
        for i in 1..u32::MAX {
            let ptr = unsafe { &mut *handle.meta.cast::<FotEntry>().sub((i + 1) as usize) };
            let flags = FotFlags::from_bits_truncate(ptr.flags.load(Ordering::SeqCst));

            if flags.contains(FotFlags::ALLOCATED)
                && flags.contains(FotFlags::ACTIVE)
                && !flags.contains(FotFlags::DELETED)
            {
                if ptr.values == new_fot.values && ptr.resolver == new_fot.resolver {
                    return Ok(i);
                }
            }

            if flags.contains(FotFlags::DELETED)
                || (!flags.contains(FotFlags::ACTIVE) && !flags.contains(FotFlags::ALLOCATED))
            {
                if let Ok(_) = ptr.flags.compare_exchange(
                    flags.bits(),
                    FotFlags::ALLOCATED.bits(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    let mut flags =
                        FotFlags::from_bits_truncate(new_fot.flags.load(Ordering::SeqCst));
                    flags.set(FotFlags::DELETED, false);
                    flags.set(FotFlags::ALLOCATED, true);
                    flags.set(FotFlags::ACTIVE, true);
                    ptr.values = new_fot.values;
                    ptr.resolver = new_fot.resolver;
                    ptr.flags.store(flags.bits(), Ordering::SeqCst);
                    return Ok(i);
                }
            }
        }
        Err(ResourceError::OutOfResources.into())
    }

    fn read_fot_entry(&self, handle: &object_handle, idx: u64) -> Result<FotEntry> {
        let ptr = unsafe { &*handle.meta.cast::<FotEntry>().sub((idx + 1) as usize) };
        let flags = FotFlags::from_bits_truncate(ptr.flags.load(Ordering::SeqCst));
        if flags.contains(FotFlags::DELETED)
            || !flags.contains(FotFlags::ACTIVE)
            || !flags.contains(FotFlags::ALLOCATED)
        {
            return Err(ObjectError::InvalidFote.into());
        }
        if flags.contains(FotFlags::RESOLVER) {
            return Err(TwzError::NOT_SUPPORTED);
        }
        let val = unsafe { (ptr as *const FotEntry).read_volatile() };

        let flags = FotFlags::from_bits_truncate(val.flags.load(Ordering::SeqCst));
        if flags.contains(FotFlags::DELETED)
            || !flags.contains(FotFlags::ACTIVE)
            || !flags.contains(FotFlags::ALLOCATED)
        {
            return Err(ObjectError::InvalidFote.into());
        }
        Ok(val)
    }

    pub fn resolve_fot(
        &self,
        handle: *mut object_handle,
        idx: u64,
        _valid_len: usize,
        map_flags: MapFlags,
    ) -> Result<ObjectHandle> {
        if idx == 0 || handle.is_null() {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let handle = unsafe { &*handle };
        tracing::trace!("Resolving FOT: {:x}", handle.id);
        let entry = self.read_fot_entry(handle, idx)?;
        let id = ObjID::from_parts(entry.values);

        let res_handle = self.map_object(id, map_flags)?;
        unsafe { &*handle.runtime_info.cast::<RuntimeHandleInfo>() }
            .fot_cache
            .insert(idx, map_flags, res_handle.clone());
        Ok(res_handle)
    }

    pub fn resolve_fot_local(
        &self,
        ptr: *mut u8,
        idx: u64,
        _valid_len: usize,
        flags: MapFlags,
    ) -> *mut u8 {
        if let Some(handle) = self.object_manager.lock().get_handle(ptr) {
            tracing::trace!("Resolving FOT local: {:x}", handle.id);
            let rtinfo: *const RuntimeHandleInfo = handle.runtime_info.cast();
            unsafe {
                return (&*rtinfo)
                    .fot_cache
                    .resolve_cached_ptr(idx, flags)
                    .unwrap_or(core::ptr::null_mut());
            }
        }
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
