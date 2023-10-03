//! Implements some helper types and functions for working with objects in this runtime.

use core::marker::PhantomData;

use twizzler_runtime_api::{MapFlags, ObjectHandle};

use crate::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    runtime::object::slot::global_allocate,
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};

#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct InternalObject<T> {
    slot: usize,
    runtime_handle: ObjectHandle,
    _pd: PhantomData<T>,
}

impl<T> InternalObject<T> {
    #[allow(dead_code)]
    pub(crate) fn create_data_and_map() -> Option<Self> {
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
            ),
            &[],
            &[],
        )
        .ok()?;
        let slot = global_allocate()?;
        let _map = sys_object_map(
            None,
            id,
            slot,
            Protections::READ | Protections::WRITE,
            crate::syscall::MapFlags::empty(),
        )
        .ok()?;
        Some(Self {
            slot,
            runtime_handle: ObjectHandle {
                id: id.as_u128(),
                flags: MapFlags::READ | MapFlags::WRITE,
                start: (slot * MAX_SIZE) as *mut u8,
                meta: (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
            },
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn base(&self) -> &T {
        let (start, _) = super::slot::slot_to_start_and_meta(self.slot);
        unsafe { ((start + NULLPAGE_SIZE) as *const T).as_ref().unwrap() }
    }

    #[allow(dead_code)]
    pub(crate) unsafe fn base_mut(&self) -> &mut T {
        let (start, _) = super::slot::slot_to_start_and_meta(self.slot);
        unsafe { ((start + NULLPAGE_SIZE) as *mut T).as_mut().unwrap() }
    }

    #[allow(dead_code)]
    pub(crate) fn id(&self) -> ObjID {
        ObjID::new(self.runtime_handle.id)
    }

    #[allow(dead_code)]
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    #[allow(dead_code)]
    pub(crate) fn map(id: ObjID, prot: Protections) -> Option<Self> {
        let slot = super::slot::global_allocate()?;
        crate::syscall::sys_object_map(None, id, slot, prot, crate::syscall::MapFlags::empty())
            .ok()?;

        Some(Self {
            runtime_handle: ObjectHandle {
                id: id.as_u128(),
                flags: prot.into(),
                start: (slot * MAX_SIZE) as *mut u8,
                meta: (slot * MAX_SIZE + MAX_SIZE - NULLPAGE_SIZE) as *mut u8,
            },
            slot,
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn offset<P>(&self, offset: usize) -> Option<*const P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.start.add(offset) as *const P })
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub(crate) fn offset_mut<P>(&mut self, offset: usize) -> Option<*mut P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.start.add(offset) as *mut P })
        } else {
            None
        }
    }
}

impl<T> Drop for InternalObject<T> {
    fn drop(&mut self) {
        // TODO
        //super::slot::global_release(self.slot);
    }
}

impl From<Protections> for MapFlags {
    fn from(p: Protections) -> Self {
        let mut f = MapFlags::empty();
        if p.contains(Protections::READ) {
            f.insert(MapFlags::READ);
        }

        if p.contains(Protections::WRITE) {
            f.insert(MapFlags::WRITE);
        }

        if p.contains(Protections::EXEC) {
            f.insert(MapFlags::EXEC);
        }
        f
    }
}
