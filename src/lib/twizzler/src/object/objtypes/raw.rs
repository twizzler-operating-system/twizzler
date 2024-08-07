use twizzler_abi::{
    meta::MetaInfo,
    object::{MAX_SIZE, NULLPAGE_SIZE},
};
use twizzler_runtime_api::{ObjID, ObjectHandle};

use crate::object::fot::FotEntry;

pub trait RawObject {
    fn handle(&self) -> &ObjectHandle;

    fn id(&self) -> ObjID {
        self.handle().id
    }

    fn base_ptr(&self) -> *const u8 {
        // TODO
        self.lea(NULLPAGE_SIZE, 0).unwrap()
    }

    fn base_mut_ptr(&self) -> *mut u8 {
        // TODO
        self.lea_mut(NULLPAGE_SIZE, 0).unwrap()
    }

    fn meta_ptr(&self) -> *const MetaInfo {
        self.handle().meta as *const _
    }

    fn meta_mut_ptr(&self) -> *mut MetaInfo {
        self.handle().meta as *mut _
    }

    fn fote_ptr(&self, idx: usize) -> Option<*const FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_ptr() as *const FotEntry).offset(-offset)) }
    }

    fn fote_ptr_mut(&self, idx: usize) -> Option<*mut FotEntry> {
        let offset: isize = (1 + idx).try_into().ok()?;
        unsafe { Some((self.meta_mut_ptr() as *mut FotEntry).offset(-offset)) }
    }

    fn lea(&self, offset: usize, _len: usize) -> Option<*const u8> {
        Some(unsafe { self.handle().start.add(offset) as *const u8 })
    }

    fn lea_mut(&self, offset: usize, _len: usize) -> Option<*mut u8> {
        Some(unsafe { self.handle().start.add(offset) as *mut u8 })
    }

    fn ptr_local(&self, ptr: *const u8) -> Option<usize> {
        if ptr.addr() >= self.handle().start.addr()
            && ptr.addr() < self.handle().start.addr() + MAX_SIZE
        {
            Some(ptr.addr() - self.handle().start.addr())
        } else {
            None
        }
    }
}

impl RawObject for ObjectHandle {
    fn handle(&self) -> &ObjectHandle {
        self
    }
}
