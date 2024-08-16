//! Implementation of the object runtime.

use core::{mem::ManuallyDrop, ptr::NonNull};

use rustc_alloc::{collections::BTreeMap, vec::Vec};
use twizzler_runtime_api::{
    InternalHandleRefs, MapError, MapFlags, ObjectHandle, ObjectRuntime, StartOrHandle,
    StartOrHandleRef,
};

use self::slot::global_release;
use super::{simple_mutex, MinimalRuntime};
use crate::{
    meta::MetaInfo,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::object::slot::global_allocate,
    rustc_alloc::boxed::Box,
    syscall::{sys_object_map, ObjectMapError, ObjectUnmapError, UnmapFlags},
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

type Mapping = (ObjID, MapFlags);

const QUEUE_LEN: usize = 32;

struct HandleCache {
    active: BTreeMap<Mapping, ManuallyDrop<ObjectHandle>>,
    queued: Vec<(Mapping, ManuallyDrop<ObjectHandle>)>,
    slotmap: BTreeMap<usize, Mapping>,
}

fn do_unmap(handle: &ObjectHandle) -> Result<(), ObjectUnmapError> {
    let slot = (handle.start as usize) / MAX_SIZE;
    // No one else has a reference outside of the runtime, and we are being called by the handle
    // cache after it has bumped us from the queue. Thus, the internal refs had to be zero (to
    // be on the queue) and never incremented ()
    unsafe {
        drop(Box::from_raw(handle.internal_refs.as_ptr()));
    }
    crate::syscall::sys_object_unmap(None, slot, UnmapFlags::empty())?;
    global_release(slot);

    Ok(())
}

impl HandleCache {
    const fn new() -> Self {
        Self {
            active: BTreeMap::new(),
            queued: Vec::new(),
            slotmap: BTreeMap::new(),
        }
    }

    /// If map is present in either the active or the inactive lists, return a mutable reference to
    /// it. If the handle was inactive, move it to the active list.
    pub fn activate(&mut self, map: Mapping) -> Option<&mut ObjectHandle> {
        let idx = self.queued.iter().position(|item| item.0 == map);
        if let Some(idx) = idx {
            let (_, handle) = self.queued.remove(idx);
            self.insert(&handle);
        }

        self.active.get_mut(&map).map(|item| &mut **item)
    }

    /// Activate, using a slot as key.
    pub fn activate_from_slot(&mut self, slot: usize) -> Option<&mut ObjectHandle> {
        let map = self.slotmap.get(&slot)?;
        self.activate(*map)
    }

    /// Insert a handle into the active list. Item must not be already mapped.
    pub fn insert(&mut self, handle: &ObjectHandle) {
        let slot = (handle.start as usize) / MAX_SIZE;
        let map = (handle.id, handle.flags);
        let _r = self.active.insert(map, Self::track(handle));
        debug_assert!(_r.is_none());
        self.slotmap.insert(slot, map);
    }

    fn do_remove(&mut self, item: ManuallyDrop<ObjectHandle>) {
        let slot = (item.start as usize) / MAX_SIZE;
        if let Err(_e) = do_unmap(&item) {
            // TODO: log the error?
        }
        self.slotmap.remove(&slot);
    }

    /// Release a handle. Must only be called from runtime handle release (internal_refs == 0).
    pub fn release(&mut self, handle: &mut ObjectHandle) {
        let map = (handle.id, handle.flags);
        if let Some(handle) = self.active.remove(&map) {
            // If queue is full, evict.
            if self.queued.len() >= QUEUE_LEN {
                let (_, old) = self.queued.remove(0);
                self.do_remove(old);
            }
            self.queued.push(((handle.id, handle.flags), handle));
        }
    }

    /// Flush all items in the inactive queue.
    pub fn flush(&mut self) {
        let to_remove = self.queued.drain(..).collect::<Vec<_>>();
        for item in to_remove {
            self.do_remove(item.1);
        }
    }

    fn track(handle: &ObjectHandle) -> ManuallyDrop<ObjectHandle> {
        // We do NOT increment internal refs, which means we can NEVER look through that pointer.
        // Everything else is copy. Since we do not increment refs, put this into a ManuallyDrop.
        ManuallyDrop::new(ObjectHandle::new(
            handle.internal_refs,
            handle.id,
            handle.flags,
            handle.start,
            handle.meta,
        ))
    }
}

static HANDLES: simple_mutex::Mutex<HandleCache> = simple_mutex::Mutex::new(HandleCache::new());

impl ObjectRuntime for MinimalRuntime {
    fn map_object(
        &self,
        id: twizzler_runtime_api::ObjID,
        flags: twizzler_runtime_api::MapFlags,
    ) -> Result<twizzler_runtime_api::ObjectHandle, twizzler_runtime_api::MapError> {
        let map = (id, flags);
        let mut handles = HANDLES.lock();
        if let Some(handle) = handles.activate(map) {
            return Ok(handle.clone());
        }

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
        handles.insert(&handle);

        Ok(handle)
    }

    fn release_handle(&self, handle: &mut twizzler_runtime_api::ObjectHandle) {
        HANDLES.lock().release(handle);
    }

    fn ptr_to_handle(&self, va: *const u8) -> Option<(ObjectHandle, usize)> {
        let (_start, offset) = self.ptr_to_object_start(va, 0)?;
        let slot = va as usize / MAX_SIZE;
        let mut handles = HANDLES.lock();
        let handle = handles.activate_from_slot(slot)?;
        Some((handle.clone(), offset))
    }

    fn ptr_to_object_start(&self, va: *const u8, _valid_len: usize) -> Option<(*const u8, usize)> {
        let slot = (va as usize) / MAX_SIZE;
        let start = slot * MAX_SIZE;
        let offset = (va as usize) - start;
        Some((start as *const u8, offset))
    }

    fn resolve_fot_to_object_start(
        &self,
        handle: StartOrHandleRef<'_>,
        idx: usize,
        _valid_len: usize,
    ) -> Result<StartOrHandle, twizzler_runtime_api::FotResolveError> {
        let start = match handle {
            StartOrHandleRef::Start(s) => s,
            StartOrHandleRef::Handle(h) => h.start,
        };
        if core::intrinsics::likely(idx == 0) {
            return Ok(StartOrHandle::Start(start));
        }

        let meta = unsafe { start.add(MAX_SIZE - NULLPAGE_SIZE) };
        let fote = unsafe {
            let fot0_ptr = (meta as *mut FotEntry).offset(-1);
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
