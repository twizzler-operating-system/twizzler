//! Implements some helper types and functions for working with objects in this runtime.

use core::marker::PhantomData;

use twizzler_abi::{
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, sys_object_map, BackingType, LifetimeType, ObjectCreate,
        ObjectCreateFlags,
    },
};
use twizzler_rt_abi::object::{MapFlags, ObjectHandle};

use super::slot::global_allocate;

#[allow(dead_code)]
pub(crate) struct InternalObject<T> {
    slot: usize,
    runtime_handle: ObjectHandle,
    _pd: PhantomData<T>,
}

impl<T> core::fmt::Debug for InternalObject<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("InternalObject")
            .field("slot", &self.slot)
            .field("runtime_handle", &self.runtime_handle)
            .finish()
    }
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
                Protections::all(),
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
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .ok()?;

        let start = (slot * MAX_SIZE) as *mut _;
        let meta = (((slot + 1) * MAX_SIZE) - NULLPAGE_SIZE) as *mut _;

        Some(Self {
            slot,
            runtime_handle: unsafe {
                ObjectHandle::new(
                    id,
                    super::new_runtime_info().cast(),
                    start,
                    meta,
                    MapFlags::READ | MapFlags::WRITE,
                    MAX_SIZE - NULLPAGE_SIZE * 2,
                )
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
        self.runtime_handle.id()
    }

    #[allow(dead_code)]
    pub(crate) fn slot(&self) -> usize {
        self.slot
    }

    #[allow(dead_code)]
    pub(crate) fn map(id: ObjID, prot: Protections) -> Option<Self> {
        let slot = super::slot::global_allocate()?;
        twizzler_abi::syscall::sys_object_map(
            None,
            id,
            slot,
            prot,
            twizzler_abi::syscall::MapFlags::empty(),
        )
        .ok()?;

        let start = (slot * MAX_SIZE) as *mut _;
        let meta = (((slot + 1) * MAX_SIZE) - NULLPAGE_SIZE) as *mut _;

        Some(Self {
            runtime_handle: unsafe {
                ObjectHandle::new(
                    id,
                    super::new_runtime_info().cast(),
                    start,
                    meta,
                    prot.into(),
                    MAX_SIZE - NULLPAGE_SIZE * 2,
                )
            },
            slot,
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn offset<P>(&self, offset: usize) -> Option<*const P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.start().add(offset) as *const P })
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub(crate) fn offset_mut<P>(&mut self, offset: usize) -> Option<*mut P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.start().add(offset) as *mut P })
        } else {
            None
        }
    }
}
