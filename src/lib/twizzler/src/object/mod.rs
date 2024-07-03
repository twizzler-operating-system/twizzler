use std::marker::PhantomData;

use twizzler_runtime_api::{MapError, MapFlags, ObjID, ObjectHandle};

use crate::{
    ptr::InvPtrBuilder,
    tx::{TxHandle, TxResult},
};

mod builder;
pub use builder::ObjectBuilder;

pub struct Object<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> Object<Base> {
    pub fn base(&self) -> BaseRef<'_, Base> {
        todo!()
    }

    pub fn open(&self, id: ObjID, flags: MapFlags) -> Result<Self, MapError> {
        todo!()
    }

    pub fn tx<TxFn, Ret, Err>(&self, txfn: TxFn) -> TxResult<Ret, Err>
    where
        TxFn: FnOnce(TxHandle<'_>) -> Result<Ret, Err>,
    {
        todo!()
    }
}

pub trait BaseType {}

pub struct BaseRef<'a, Base: BaseType> {
    ptr: &'a Base,
}

impl<'a, Base: BaseType> From<BaseRef<'a, Base>> for InvPtrBuilder<Base> {
    fn from(value: BaseRef<'a, Base>) -> Self {
        todo!()
    }
}

impl<'a, Base: BaseType> std::ops::Deref for BaseRef<'a, Base> {
    type Target = Base;

    fn deref(&self) -> &Self::Target {
        self.ptr
    }
}
