use std::{
    cell::Cell,
    marker::PhantomData,
    ops::{Deref, DerefMut, Index, IndexMut, RangeBounds},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::GlobalPtr;
use crate::{object::RawObject, tx::TxHandle};

pub struct Ref<'obj, T> {
    ptr: *const T,
    handle: Cell<*const ObjectHandle>,
    owned: Cell<bool>,
    _pd: PhantomData<&'obj T>,
}

impl<'obj, T> Ref<'obj, T> {
    pub fn raw(&self) -> *const T {
        self.ptr
    }

    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn handle(&self) -> &ObjectHandle {
        if self.handle.get().is_null() {
            let handle =
                twizzler_rt_abi::object::twz_rt_get_object_handle(self.ptr.cast()).unwrap();
            self.handle.set(Box::into_raw(Box::new(handle)));
            self.owned.set(true);
        }

        unsafe { self.handle.get().as_ref().unwrap_unchecked() }
    }

    pub unsafe fn from_raw_parts(ptr: *const T, handle: *const ObjectHandle) -> Self {
        Self {
            ptr,
            handle: Cell::new(handle),
            owned: Cell::new(false),
            _pd: PhantomData,
        }
    }

    pub unsafe fn cast<U>(self) -> Ref<'obj, U> {
        let ret = Ref {
            ptr: self.ptr.cast(),
            handle: Cell::new(self.handle.get()),
            owned: Cell::new(self.owned.get()),
            _pd: PhantomData,
        };
        std::mem::forget(self);
        ret
    }

    unsafe fn mutable_to(self, ptr: *mut T) -> RefMut<'obj, T> {
        let ret = RefMut {
            ptr,
            handle: Cell::new(self.handle.get()),
            owned: Cell::new(self.owned.get()),
            _pd: PhantomData,
        };
        std::mem::forget(self);
        ret
    }

    pub unsafe fn mutable(self) -> RefMut<'obj, T> {
        let ptr = self.ptr as *mut T;
        self.mutable_to(ptr)
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    pub fn owned<'b>(&self) -> Ref<'b, T> {
        Ref {
            ptr: self.ptr,
            owned: Cell::new(true),
            handle: Cell::new(Box::into_raw(Box::new(self.handle().clone()))),
            _pd: PhantomData,
        }
    }

    pub fn from_handle(handle: ObjectHandle, ptr: *const T) -> Self {
        Self {
            ptr,
            owned: Cell::new(true),
            handle: Cell::new(Box::into_raw(Box::new(handle))),
            _pd: PhantomData,
        }
    }

    pub fn tx(self, tx: &(impl TxHandle + 'obj)) -> crate::tx::Result<RefMut<'obj, T>> {
        let ptr = tx.tx_mut(self.ptr.cast(), size_of::<T>())?;
        Ok(unsafe { self.mutable_to(ptr.cast()) })
    }
}

impl<'obj, T: core::fmt::Debug> core::fmt::Debug for Ref<'obj, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.deref())
    }
}

impl<'obj, T> Deref for Ref<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'a, T> From<Ref<'a, T>> for GlobalPtr<T> {
    fn from(value: Ref<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'a, T> Drop for Ref<'a, T> {
    fn drop(&mut self) {
        if self.owned.get() {
            let _boxed = unsafe { Box::from_raw(self.handle.get() as *mut ObjectHandle) };
        }
    }
}

pub struct RefMut<'obj, T> {
    ptr: *mut T,
    handle: Cell<*const ObjectHandle>,
    owned: Cell<bool>,
    _pd: PhantomData<&'obj mut T>,
}

impl<'obj, T> RefMut<'obj, T> {
    pub fn raw(&self) -> *mut T {
        self.ptr
    }

    pub unsafe fn from_raw_parts(ptr: *mut T, handle: *const ObjectHandle) -> Self {
        Self {
            ptr,
            handle: Cell::new(handle),
            owned: Cell::new(false),
            _pd: PhantomData,
        }
    }

    pub unsafe fn cast<U>(self) -> RefMut<'obj, U> {
        let ret = RefMut {
            ptr: self.ptr.cast(),
            handle: Cell::new(self.handle.get()),
            owned: Cell::new(self.owned.get()),
            _pd: PhantomData,
        };
        std::mem::forget(self);
        ret
    }

    pub fn handle(&self) -> &ObjectHandle {
        if self.handle.get().is_null() {
            let handle =
                twizzler_rt_abi::object::twz_rt_get_object_handle(self.ptr.cast()).unwrap();
            self.handle.set(Box::into_raw(Box::new(handle)));
            self.owned.set(true);
        }
        unsafe { self.handle.get().as_ref().unwrap_unchecked() }
    }

    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    pub fn owned<'b>(&self) -> RefMut<'b, T> {
        RefMut {
            ptr: self.ptr,
            owned: Cell::new(true),
            handle: Cell::new(Box::into_raw(Box::new(self.handle().clone()))),
            _pd: PhantomData,
        }
    }
}

impl<'obj, T: core::fmt::Debug> core::fmt::Debug for RefMut<'obj, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.deref())
    }
}

impl<'obj, T> Deref for RefMut<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T> DerefMut for RefMut<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<'a, T> From<RefMut<'a, T>> for GlobalPtr<T> {
    fn from(value: RefMut<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

impl<'a, T> Drop for RefMut<'a, T> {
    fn drop(&mut self) {
        if self.owned.get() {
            let _boxed = unsafe { Box::from_raw(self.handle.get() as *mut ObjectHandle) };
        }
    }
}

pub struct RefSlice<'a, T> {
    ptr: Ref<'a, T>,
    len: usize,
}

impl<'a, T> RefSlice<'a, T> {
    pub unsafe fn from_ref(ptr: Ref<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn as_slice(&self) -> &[T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts(raw_ptr, self.len) }
    }

    pub fn get(&self, idx: usize) -> Option<Ref<'_, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_raw_parts(ptr, self.ptr.handle.get()) })
    }

    pub fn get_into(self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)? as *const T;
        let mut r = self.ptr;
        r.ptr = ptr;
        Some(r)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn tx(
        self,
        range: impl RangeBounds<usize>,
        tx: &(impl TxHandle + 'a),
    ) -> crate::tx::Result<RefSliceMut<'a, T>> {
        let start = match range.start_bound() {
            std::ops::Bound::Included(n) => *n,
            std::ops::Bound::Excluded(n) => n.saturating_add(1),
            std::ops::Bound::Unbounded => 0,
        };
        let end = match range.start_bound() {
            std::ops::Bound::Included(n) => n.saturating_add(1),
            std::ops::Bound::Excluded(n) => *n,
            std::ops::Bound::Unbounded => self.len,
        };
        let len = end - start;
        unsafe {
            let ptr = tx.tx_mut(self.ptr.ptr.add(start).cast(), size_of::<T>() * len)?;
            let r = self.ptr.mutable_to(ptr.cast());
            Ok(RefSliceMut::from_ref(r, len))
        }
    }
}

impl<'a, T> Index<usize> for RefSlice<'a, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

pub struct RefSliceMut<'a, T> {
    ptr: RefMut<'a, T>,
    len: usize,
}

impl<'a, T> RefSliceMut<'a, T> {
    pub unsafe fn from_ref(ptr: RefMut<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    pub fn as_slice(&self) -> &[T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts(raw_ptr, self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &mut [T] {
        let raw_ptr = self.ptr.raw();
        unsafe { core::slice::from_raw_parts_mut(raw_ptr, self.len) }
    }

    pub fn get(&self, idx: usize) -> Option<Ref<'_, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_raw_parts(ptr, self.ptr.handle.get()) })
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<RefMut<'_, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)?;
        Some(unsafe { RefMut::from_raw_parts(ptr, self.ptr.handle.get()) })
    }

    pub fn len(&self) -> usize {
        self.len
    }
}

impl<'a, T> Index<usize> for RefSliceMut<'a, T> {
    type Output = T;

    fn index(&self, index: usize) -> &Self::Output {
        let slice = self.as_slice();
        &slice[index]
    }
}

impl<'a, T> IndexMut<usize> for RefSliceMut<'a, T> {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let slice = self.as_slice_mut();
        &mut slice[index]
    }
}
