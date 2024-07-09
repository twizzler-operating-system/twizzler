use twizzler_abi::{meta::MetaInfo, object::NULLPAGE_SIZE};
use twizzler_runtime_api::{ObjID, ObjectHandle};

use crate::object::fot::FotEntry;

pub trait RawObject {
    fn handle(&self) -> &ObjectHandle;

    fn id(&self) -> ObjID {
        self.handle().id
    }

    fn base_ptr(&self) -> *const u8 {
        unsafe { self.handle().start.add(NULLPAGE_SIZE) }
    }

    fn base_mut_ptr(&self) -> *mut u8 {
        unsafe { self.handle().start.add(NULLPAGE_SIZE) }
    }

    fn meta_ptr(&self) -> *const MetaInfo {
        self.handle().meta as *const _
    }

    fn meta_mut_ptr(&self) -> *mut MetaInfo {
        self.handle().meta as *mut _
    }

    fn fote_ptr(&self, idx: usize) -> Option<*const FotEntry> {
        todo!()
    }

    fn fote_ptr_mut(&self, idx: usize) -> Option<*mut FotEntry> {
        todo!()
    }
}
