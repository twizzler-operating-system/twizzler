use std::{borrow::Borrow, marker::PhantomData, mem::MaybeUninit};

use twizzler_rt_abi::object::ObjectHandle;

use super::{Result, TxHandle};
use crate::{
    alloc::{invbox::InvBox, Allocator, OwnedGlobalPtr},
    marker::{BaseType, Invariant},
    object::{FotEntry, Object, RawObject, TypedObject},
    ptr::RefMut,
};

#[repr(C)]
pub struct TxObject<T = ()> {
    handle: ObjectHandle,
    _pd: PhantomData<*mut T>,
}

impl<T> TxObject<T> {
    pub fn new(object: Object<T>) -> Result<Self> {
        // TODO: start tx
        Ok(Self {
            handle: object.into_handle(),
            _pd: PhantomData,
        })
    }

    pub fn commit(self) -> Result<Object<T>> {
        // TODO: commit tx
        Ok(unsafe { Object::from_handle_unchecked(self.handle) })
    }

    pub fn abort(self) -> Object<T> {
        // TODO: abort tx
        unsafe { Object::from_handle_unchecked(self.handle) }
    }

    pub fn base_mut(&mut self) -> RefMut<'_, T> {
        // TODO: track base in tx
        unsafe { RefMut::from_raw_parts(self.base_mut_ptr(), self.handle()) }
    }

    pub fn insert_fot(&self, fot: FotEntry) -> crate::tx::Result<u64> {
        todo!()
    }

    pub fn into_unit(self) -> TxObject<()> {
        TxObject {
            handle: self.handle,
            _pd: PhantomData,
        }
    }
}

impl<B> TxObject<MaybeUninit<B>> {
    pub fn write(self, baseval: B) -> crate::tx::Result<TxObject<B>> {
        let base = unsafe { self.base_mut_ptr::<MaybeUninit<B>>().as_mut().unwrap() };
        base.write(baseval);
        TxObject::new(unsafe { Object::from_handle_unchecked(self.handle) })
    }
}

impl<B> TxHandle for TxObject<B> {
    fn tx_mut(&self, data: *const u8, len: usize) -> super::Result<*mut u8> {
        // TODO
        Ok(data as *mut u8)
    }
}

impl<T> RawObject for TxObject<T> {
    fn handle(&self) -> &twizzler_rt_abi::object::ObjectHandle {
        &self.handle
    }
}

impl<B: BaseType> TypedObject for TxObject<B> {
    type Base = B;

    fn base(&self) -> crate::ptr::Ref<'_, Self::Base> {
        unsafe { crate::ptr::Ref::from_raw_parts(self.base_ptr(), self.handle()) }
    }
}

impl<B> AsRef<TxObject<()>> for TxObject<B> {
    fn as_ref(&self) -> &TxObject<()> {
        let this = self as *const Self;
        // Safety: This phantom data is the only generic field, and we are repr(C).
        unsafe { this.cast::<TxObject<()>>().as_ref().unwrap() }
    }
}

mod tests {
    use crate::{
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
    };

    struct Simple {
        x: u32,
    }

    impl BaseType for Simple {}

    fn single_tx() {
        let builder = ObjectBuilder::default();
        let obj = builder.build(Simple { x: 3 }).unwrap();
        let base = obj.base();
        assert_eq!(base.x, 3);
        drop(base);

        let mut tx = obj.tx().unwrap();
        let mut base = tx.base_mut();
        base.x = 42;
        let obj = tx.commit().unwrap();
        assert_eq!(obj.base().x, 42);
    }
}
