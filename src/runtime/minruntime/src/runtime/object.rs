//! Implementation of the object runtime.

use core::{ptr::NonNull, sync::atomic::AtomicU64};

use rustc_alloc::boxed::Box;
use slot::global_allocate;
use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{sys_object_map, ObjectMapError, UnmapFlags},
};
use twizzler_rt_abi::object::{MapError, MapFlags, ObjectHandle};

use super::MinimalRuntime;

mod handle;

#[allow(unused_imports)]
pub use handle::*;

pub(crate) mod slot;

#[repr(C)]
struct RuntimeHandleInfo {
    refs: AtomicU64,
}

pub(crate) fn new_runtime_info() -> *mut RuntimeHandleInfo {
    let rhi = Box::new(RuntimeHandleInfo {
        refs: AtomicU64::new(1),
    });
    Box::into_raw(rhi)
}

impl MinimalRuntime {
    pub fn map_object(&self, id: ObjID, flags: MapFlags) -> Result<ObjectHandle, MapError> {
        let slot = global_allocate().ok_or(MapError::OutOfResources)?;
        let _ = sys_object_map(None, id, slot, flags.into(), flags.into()).map_err(|e| e.into())?;
        let start = (slot * MAX_SIZE) as *mut _;
        let meta = (((slot + 1) * MAX_SIZE) - NULLPAGE_SIZE) as *mut _;
        Ok(unsafe {
            ObjectHandle::new(
                id,
                new_runtime_info().cast(),
                start,
                meta,
                flags,
                MAX_SIZE - NULLPAGE_SIZE * 2,
            )
        })
    }

    pub fn release_handle(&self, handle: *mut twizzler_rt_abi::bindings::object_handle) {
        let slot = (unsafe { (*handle).start } as usize) / MAX_SIZE;

        if twizzler_abi::syscall::sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
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
