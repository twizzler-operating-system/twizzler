use core::marker::PhantomData;

use twizzler_runtime_api::{MapFlags, ObjectHandle};

use crate::{
    arch::to_vaddr_range,
    object::{ObjID, Protections, MAX_SIZE, NULLPAGE_SIZE},
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
        todo!()
    }

    #[allow(dead_code)]
    pub(crate) fn base(&self) -> &T {
        let (start, _) = to_vaddr_range(self.slot);
        unsafe { (start as *const T).as_ref().unwrap() }
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
                base: (slot * MAX_SIZE) as *mut u8,
            },
            slot,
            _pd: PhantomData,
        })
    }

    #[allow(dead_code)]
    pub(crate) fn offset<P>(&self, offset: usize) -> Option<*const P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.base.add(offset) as *const P })
        } else {
            None
        }
    }

    #[allow(dead_code)]
    pub(crate) fn offset_mut<P>(&mut self, offset: usize) -> Option<*mut P> {
        if offset >= NULLPAGE_SIZE && offset < MAX_SIZE {
            Some(unsafe { self.runtime_handle.base.add(offset) as *mut P })
        } else {
            None
        }
    }
}

impl<T> Drop for InternalObject<T> {
    fn drop(&mut self) {
        super::slot::global_release(self.slot);
    }
}

impl From<Protections> for MapFlags {
    fn from(_: Protections) -> Self {
        todo!()
    }
}
