//! Implementation of the object runtime.

use core::sync::atomic::AtomicU64;

use rustc_alloc::{boxed::Box, vec::Vec};
use slot::{global_allocate, global_allocate_run};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID},
    syscall::{UnmapFlags, sys_object_map, sys_object_unmap},
};
use twizzler_rt_abi::{
    Result,
    error::{ResourceError, TwzError},
    object::{MapFlags, ObjectHandle},
};

use super::MinimalRuntime;

mod handle;

#[allow(unused_imports)]
pub use handle::*;

pub(crate) mod slot;

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

fn new_handle(id: ObjID, slot: usize, flags: MapFlags) -> ObjectHandle {
    let start = (slot * MAX_SIZE) as *mut _;
    let meta = (((slot + 1) * MAX_SIZE) - NULLPAGE_SIZE) as *mut _;
    unsafe {
        ObjectHandle::new(
            id,
            new_runtime_info().cast(),
            start,
            meta,
            flags,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }
}

impl MinimalRuntime {
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle> {
        let slot = global_allocate().ok_or(ResourceError::OutOfResources)?;
        let _ = sys_object_map(None, id, slot, flags.into(), flags.into())?;
        Ok(new_handle(id, slot, flags))
    }

    pub fn release_handle(
        &self,
        handle: *mut twizzler_rt_abi::bindings::object_handle,
        _flags: twizzler_rt_abi::bindings::release_flags,
    ) {
        let slot = (unsafe { (*handle).start } as usize) / MAX_SIZE;

        if twizzler_abi::syscall::sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
            slot::global_release(slot);
        }
    }

    /// Map two objects in sequence, useful for executable loading.
    pub fn map_two_objects(
        &self,
        in_id_a: ObjID,
        in_flags_a: MapFlags,
        in_id_b: ObjID,
        in_flags_b: MapFlags,
    ) -> Result<(ObjectHandle, ObjectHandle)> {
        let mut handles = self
            .map_adjacent_objects(&[(in_id_a, in_flags_a), (in_id_b, in_flags_b)])?
            .into_iter();
        Ok((handles.next().unwrap(), handles.next().unwrap()))
    }

    /// Map objects into adjacent slots, in order. All or nothing.
    pub fn map_adjacent_objects(&self, objs: &[(ObjID, MapFlags)]) -> Result<Vec<ObjectHandle>> {
        if objs.is_empty() {
            return Err(TwzError::INVALID_ARGUMENT);
        }
        let base = global_allocate_run(objs.len()).ok_or(ResourceError::OutOfResources)?;
        for (i, (id, flags)) in objs.iter().enumerate() {
            if let Err(e) = sys_object_map(None, *id, base + i, (*flags).into(), (*flags).into()) {
                for slot in base..base + objs.len() {
                    if slot >= base + i || sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok()
                    {
                        slot::global_release(slot);
                    }
                }
                return Err(e);
            }
        }
        Ok(objs
            .iter()
            .enumerate()
            .map(|(i, (id, flags))| new_handle(*id, base + i, *flags))
            .collect())
    }
}
