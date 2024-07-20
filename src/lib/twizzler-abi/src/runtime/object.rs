//! Implementation of the object runtime.

use core::{mem::ManuallyDrop, ptr::NonNull};

use rustc_alloc::collections::BTreeMap;
use twizzler_runtime_api::{
    InternalHandleRefs, MapError, MapFlags, ObjectHandle, ObjectRuntime, StartOrHandle,
};

use super::{simple_mutex, MinimalRuntime};
use crate::{
    klog_println,
    meta::MetaInfo,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    print_err,
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
        klog_println!("mapping:: {}", slot);
        HANDLE_MAP.lock().insert(slot, our_handle);

        Ok(handle)
    }

    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        let slot = (handle.start as usize) / MAX_SIZE;

        // This does not run drop on the handle, which is important, since we this map does not hold
        // a counted reference.
        klog_println!("unmapping:: {}", slot);
        if let Some(item) = HANDLE_MAP.lock().remove(&slot) {
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

    fn ptr_to_handle(&self, va: *const u8) -> Option<(ObjectHandle, usize)> {
        let (start, offset) = self.ptr_to_object_start(va, 0)?;
        klog_println!("ptr_to_handle: {:p} {:p}", va, start);
        let hmap = HANDLE_MAP.lock();
        let slot = va as usize / MAX_SIZE;
        klog_println!("lookup slot {}: {}", slot, hmap.contains_key(&slot));
        let our_handle = hmap.get(&slot)?;

        // Clone will kick up the refcount again.
        let handle = ManuallyDrop::into_inner(our_handle.clone());
        Some((handle, offset))
    }

    fn ptr_to_object_start(&self, va: *const u8, valid_len: usize) -> Option<(*const u8, usize)> {
        let slot = (va as usize) / MAX_SIZE;
        let start = slot * MAX_SIZE;
        let offset = (va as usize) - start;
        Some((start as *const u8, offset))
    }

    fn resolve_fot_to_object_start<'a>(
        &self,
        handle: &'a ObjectHandle,
        idx: usize,
        valid_len: usize,
    ) -> Result<StartOrHandle, twizzler_runtime_api::FotResolveError> {
        if idx == 0 {
            return Ok(StartOrHandle::Start(handle.start));
        }

        let fote = unsafe {
            let fot0_ptr = (handle.meta as *mut FotEntry).offset(-1);
            let fot_ptr = fot0_ptr.offset(-(idx as isize));
            fot_ptr.read()
        };
        let id = ObjID::new_from_parts(fote.vals[0], fote.vals[1]);

        let handle = self.map_object(id, MapFlags::READ | MapFlags::WRITE)?;
        Ok(StartOrHandle::Handle(handle))
    }

    fn add_fot_entry(&self, handle: &ObjectHandle) -> Option<(*mut u8, usize)> {
        unsafe {
            let mut meta = (handle.meta as *const MetaInfo).read();
            let next = meta.fotcount + 1;
            meta.fotcount = next;
            (handle.meta as *mut MetaInfo).write(meta);

            let fot0_ptr = (handle.meta as *mut FotEntry).offset(-1);
            let fot_ptr = fot0_ptr.offset(-(next as isize));
            Some((fot_ptr as *mut u8, next as usize))
        }
    }
}
#[repr(C)]
struct FotEntry {
    vals: [u64; 4],
}
