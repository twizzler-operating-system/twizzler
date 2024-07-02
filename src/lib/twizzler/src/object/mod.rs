use std::marker::PhantomData;

use twizzler_runtime_api::{MapError, MapFlags, ObjID, ObjectHandle};

use crate::tx::{TxHandle, TxResult};

pub struct Object<Base: BaseType> {
    handle: ObjectHandle,
    _pd: PhantomData<*const Base>,
}

impl<Base: BaseType> Object<Base> {
    pub fn base(&self) -> &Base {
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
