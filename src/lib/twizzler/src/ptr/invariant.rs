use std::marker::PhantomData;

use twizzler_abi::object::MAX_SIZE;
use twizzler_rt_abi::object::ObjectHandle;

use super::{GlobalPtr, Ref};
use crate::{
    marker::{Invariant, PhantomStoreEffect},
    object::{FotEntry, RawObject},
    tx::{Result, TxHandle, TxObject},
};

#[repr(C)]
#[derive(Debug, PartialEq, PartialOrd, Ord, Eq)]
pub struct InvPtr<T: Invariant> {
    value: u64,
    _pse: PhantomStoreEffect,
    _pd: PhantomData<*const T>,
}

impl<T: Invariant> InvPtr<T> {
    fn get_this(this: *const Self) -> ObjectHandle {
        twizzler_rt_abi::object::twz_rt_get_object_handle(this.cast()).unwrap()
    }

    pub fn global(&self) -> GlobalPtr<T> {
        let fote = self.fot_index();
        let obj = Self::get_this(self);
        if fote == 0 {
            return GlobalPtr::new(obj.id(), self.offset());
        }
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE).unwrap();
        GlobalPtr::new(re.id(), self.offset())
    }

    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        let fote = self.fot_index();
        let obj = Self::get_this(self);
        if fote == 0 {
            // TODO: this is inefficient
            let ptr = obj
                .lea(self.offset() as usize, size_of::<T>())
                .unwrap()
                .cast();
            return Ref::from_handle(obj, ptr);
        }
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE).unwrap();
        let ptr = re
            .lea(self.offset() as usize, size_of::<T>())
            .unwrap()
            .cast();
        Ref::from_handle(re, ptr)
    }

    pub fn null() -> Self {
        Self::from_raw_parts(0, 0)
    }

    pub fn from_raw_parts(idx: u64, offset: u64) -> Self {
        Self {
            value: (idx << 48) | offset,
            _pse: PhantomStoreEffect,
            _pd: PhantomData,
        }
    }

    pub fn fot_index(&self) -> u64 {
        self.value >> 48
    }

    pub fn offset(&self) -> u64 {
        self.value & ((1 << 48) - 1)
    }

    pub fn raw(&self) -> u64 {
        self.value
    }

    pub fn new<B>(tx: &TxObject<B>, gp: impl Into<GlobalPtr<T>>) -> crate::tx::Result<Self> {
        let gp = gp.into();
        if gp.id() == tx.id() {
            return Ok(Self::from_raw_parts(0, gp.offset()));
        }
        let fote = tx.insert_fot(&gp.into())?;
        Ok(Self::from_raw_parts(fote, gp.offset()))
    }
}
