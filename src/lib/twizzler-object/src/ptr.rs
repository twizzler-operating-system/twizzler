use std::{marker::PhantomData, sync::Arc};

use crate::{
    cell::TxCell,
    slot::{vaddr_to_slot, Slot},
    tx::{TxError, TxHandle},
    Object, ObjectInitError,
};

#[repr(transparent)]
pub struct InvPtr<T> {
    raw: TxCell<u64>,
    _pd: PhantomData<T>,
}

impl<T> !Unpin for InvPtr<T> {}

impl<T> Object<T> {
    #[inline]
    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        self.slot.raw_lea(off)
    }

    #[inline]
    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        self.slot.raw_lea_mut(off)
    }

    #[inline]
    pub fn ptr_lea<'a, Target>(
        &'a self,
        ptr: InvPtr<Target>,
        tx: &impl TxHandle,
    ) -> Result<EffAddr<Target>, LeaError> {
        ptr.lea_obj(self, tx)
    }
}

pub struct EffAddr<T> {
    ptr: *const T,
    slot: Arc<Slot>,
}

fn ipoffset(raw: u64) -> u64 {
    raw & 0x0000ffffffffffff
}

fn ipfote(raw: u64) -> u64 {
    raw & 0x0000ffffffffffff
}

pub enum LeaError {
    Tx(TxError),
    Init(ObjectInitError),
}

impl From<TxError> for LeaError {
    fn from(txe: TxError) -> Self {
        Self::Tx(txe)
    }
}

impl From<ObjectInitError> for LeaError {
    fn from(init: ObjectInitError) -> Self {
        Self::Init(init)
    }
}

impl<Target> InvPtr<Target> {
    pub fn parts(&self, tx: &impl TxHandle) -> Result<(usize, u64), TxError> {
        let raw = self.raw.get(tx)?;
        Ok((ipfote(*raw) as usize, ipoffset(*raw)))
    }

    pub fn lea_obj<T>(
        &self,
        obj: &Object<T>,
        tx: &impl TxHandle,
    ) -> Result<EffAddr<Target>, LeaError> {
        assert!(self as *const Self as usize >= obj.slot.vaddr_start());
        assert!((self as *const Self as usize) < obj.slot.vaddr_meta());

        tx.ptr_resolve(self, &obj.slot)
    }

    pub fn lea(&self, tx: &impl TxHandle) -> Result<EffAddr<Target>, LeaError> {
        let slot = vaddr_to_slot(self as *const Self as usize);
        tx.ptr_resolve(self, &slot)
    }
}

impl<T> EffAddr<T> {
    pub fn obj<Base>(&self) -> Object<Base> {
        self.slot.clone().into()
    }

    pub fn new(slot: Arc<Slot>, ptr: *const T) -> Self {
        Self { ptr, slot }
    }
}

impl<T> std::ops::Deref for EffAddr<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}
