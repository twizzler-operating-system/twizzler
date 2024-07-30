use std::marker::PhantomData;

use twizzler_abi::meta::MetaInfo;
use twizzler_runtime_api::ObjectHandle;

use super::{ImmutableObject, InitializedObject, MutableObject, RawObject};
use crate::{object::BaseType, ptr::ResolvedPtr};

pub struct Object<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> Object<Base> {
    pub(crate) unsafe fn new(handle: ObjectHandle) -> Self {
        Self {
            handle,
            _pd: PhantomData,
        }
    }

    pub unsafe fn base_mut(&self) -> &mut Base {
        (self.base_mut_ptr() as *mut Base)
            .as_mut()
            .unwrap_unchecked()
    }

    // TODO: error type
    pub fn mutable(self) -> Result<MutableObject<Base>, ()> {
        todo!()
    }

    /// Convert this handle to an immutable handle.
    ///
    /// This function treats the current object handle as immutable, without creating a new
    /// immutable object. This means that this underlying handle will not have any data modified,
    /// but the underlying memory object may still change. If you want to create a new, truely
    /// immutable object, see [InitializedObject::freeze].
    pub fn immutable(self) -> ImmutableObject<Base> {
        todo!()
    }
}

impl<Base: BaseType> InitializedObject for Object<Base> {
    type Base = Base;

    fn base_ref(&self) -> &Base {
        let base = self.base_ptr() as *const Base;
        unsafe { base.as_ref().unwrap() }
    }

    fn base(&self) -> ResolvedPtr<'_, Self::Base> {
        unsafe { ResolvedPtr::new(self.base_ptr() as *const Base) }
    }

    fn meta(&self) -> &MetaInfo {
        todo!()
    }

    fn freeze(&self) -> Result<ImmutableObject<Self::Base>, ()> {
        todo!()
    }
}

impl<Base: BaseType> RawObject for Object<Base> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<Base: BaseType> Into<ObjectHandle> for Object<Base> {
    fn into(self) -> ObjectHandle {
        self.handle
    }
}
