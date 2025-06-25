use std::{
    borrow::{Borrow, BorrowMut},
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::RefMut;
use crate::{
    object::{RawObject, TxObject},
    ptr::GlobalPtr,
};

pub struct TxRef<T> {
    ptr: *mut T,
    tx: Option<TxObject<()>>,
    sync_on_drop: bool,
}

impl<T> TxRef<T> {
    pub fn as_mut(&mut self) -> RefMut<'_, T> {
        let handle = self.tx.as_ref().unwrap().handle().handle();
        unsafe { RefMut::from_raw_parts(self.ptr, handle) }
    }

    pub unsafe fn from_raw_parts<B>(tx: TxObject<B>, ptr: *mut T) -> Self {
        Self {
            ptr,
            sync_on_drop: !tx.is_nosync(),
            tx: Some(tx.into_unit()),
        }
    }

    #[inline]
    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn tx(&self) -> &TxObject<()> {
        self.tx.as_ref().unwrap()
    }

    pub fn tx_mut(&mut self) -> &mut TxObject<()> {
        self.tx.as_mut().unwrap()
    }

    pub fn into_tx(mut self) -> TxObject<()> {
        let mut txobj = self.tx.take().unwrap();
        if !self.sync_on_drop {
            txobj.nosync();
        }
        self.nosync();
        txobj
    }

    pub fn raw(&self) -> *mut T {
        self.ptr
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.tx().handle()
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    pub unsafe fn cast<U>(mut self) -> TxRef<U> {
        let old_sod = self.sync_on_drop;
        self.sync_on_drop = false;
        let ptr = self.ptr.cast::<U>();
        let mut new = TxRef::from_raw_parts(self.into_tx(), ptr);
        if !old_sod {
            new.nosync();
        }
        new
    }

    pub(crate) fn nosync(&mut self) {
        self.sync_on_drop = false;
    }

    #[allow(dead_code)]
    pub(crate) fn is_nosync(&self) -> bool {
        !self.sync_on_drop
    }
}

impl<T> TxRef<MaybeUninit<T>> {
    pub fn write(mut self, val: T) -> crate::Result<TxRef<T>> {
        unsafe {
            let ptr = self.ptr.as_mut().unwrap_unchecked();
            let tx = self.tx.take().unwrap();
            let mut new = TxRef::<T>::from_raw_parts(tx, ptr.write(val));
            if !self.sync_on_drop {
                new.nosync();
            }
            self.sync_on_drop = false;
            Ok(new)
        }
    }
}

impl<T> From<TxRef<T>> for GlobalPtr<T> {
    fn from(value: TxRef<T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<T> Deref for TxRef<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<T> DerefMut for TxRef<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<T> AsMut<T> for TxRef<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut *self
    }
}

impl<T> Borrow<T> for TxRef<T> {
    fn borrow(&self) -> &T {
        &*self
    }
}

impl<T> BorrowMut<T> for TxRef<T> {
    fn borrow_mut(&mut self) -> &mut T {
        &mut *self
    }
}

impl<T> Drop for TxRef<T> {
    #[track_caller]
    fn drop(&mut self) {
        tracing::trace!(
            "TxRef {:?} drop from {}: {} {}",
            self.tx.as_ref().map(|t| t.id()),
            core::panic::Location::caller(),
            self.sync_on_drop,
            self.tx.is_some()
        );
        let _ = self.tx.take().map(|mut tx| {
            if self.sync_on_drop {
                tx.commit()
            } else {
                Ok(())
            }
        });
    }
}

impl<T> Into<ObjectHandle> for TxRef<T> {
    fn into(self) -> ObjectHandle {
        self.tx().handle().clone()
    }
}

impl<T> Into<ObjectHandle> for &TxRef<T> {
    fn into(self) -> ObjectHandle {
        self.tx().handle().clone()
    }
}

impl<T> AsRef<ObjectHandle> for TxRef<T> {
    fn as_ref(&self) -> &ObjectHandle {
        self.tx().handle()
    }
}
