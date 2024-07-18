use twizzler_abi::object::NULLPAGE_SIZE;
use twizzler_runtime_api::ObjectHandle;

use super::InitializedObject;
use crate::ptr::{GlobalPtr, InvPtrBuilder};

pub trait BaseType {}

pub struct BaseRef<'a, Base: BaseType> {
    handle: &'a ObjectHandle,
    ptr: &'a Base,
}

impl<'a, Base: BaseType> BaseRef<'a, Base> {
    pub(crate) fn new<Obj>(handle: &'a Obj) -> Self
    where
        Obj: InitializedObject<Base = Base>,
    {
        let ptr = unsafe { (handle.base_ptr() as *const Base).as_ref().unwrap() };
        Self {
            handle: handle.handle(),
            ptr,
        }
    }
}

impl<'a, Base: BaseType> From<BaseRef<'a, Base>> for InvPtrBuilder<Base> {
    fn from(value: BaseRef<'a, Base>) -> Self {
        // TODO
        unsafe { InvPtrBuilder::from_global(GlobalPtr::new(value.handle.id, NULLPAGE_SIZE as u64)) }
    }
}

impl<'a, Base: BaseType> std::ops::Deref for BaseRef<'a, Base> {
    type Target = Base;

    fn deref(&self) -> &Self::Target {
        self.ptr
    }
}
