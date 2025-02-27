use std::{
    borrow::Cow,
    cell::{Cell, OnceCell},
    marker::PhantomData,
    ops::{Deref, DerefMut, Index, IndexMut, RangeBounds},
};

use twizzler_rt_abi::object::ObjectHandle;

use super::GlobalPtr;
use crate::{object::RawObject, tx::TxHandle};

#[derive(Default, Clone)]
struct LazyHandle<'obj> {
    handle: OnceCell<Cow<'obj, ObjectHandle>>,
}

impl<'obj> LazyHandle<'obj> {
    fn handle(&self, ptr: *const u8) -> &ObjectHandle {
        self.handle.get_or_init(|| {
            let handle = twizzler_rt_abi::object::twz_rt_get_object_handle(ptr).unwrap();
            Cow::Owned(handle)
        })
    }

    fn new_owned(handle: ObjectHandle) -> Self {
        Self {
            handle: OnceCell::from(Cow::Owned(handle)),
        }
    }

    fn new_borrowed(handle: &'obj ObjectHandle) -> Self {
        Self {
            handle: OnceCell::from(Cow::Borrowed(handle)),
        }
    }
}

pub struct Ref<'obj, T> {
    ptr: *const T,
    lazy_handle: LazyHandle<'obj>,
    _pd: PhantomData<&'obj T>,
}

impl<'obj, T> Ref<'obj, T> {
    fn new(ptr: *const T, lazy_handle: LazyHandle<'obj>) -> Self {
        Self {
            ptr,
            lazy_handle,
            _pd: PhantomData,
        }
    }

    #[inline]
    pub fn raw(&self) -> *const T {
        self.ptr
    }

    #[inline]
    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.lazy_handle.handle(self.ptr.cast())
    }

    #[inline]
    pub unsafe fn from_raw_parts(ptr: *const T, handle: &'obj ObjectHandle) -> Self {
        Self::new(ptr, LazyHandle::new_borrowed(handle))
    }

    #[inline]
    pub unsafe fn from_ptr(ptr: *const T) -> Self {
        Self::new(ptr, LazyHandle::default())
    }

    #[inline]
    pub unsafe fn cast<U>(self) -> Ref<'obj, U> {
        Ref::new(self.ptr.cast(), self.lazy_handle)
    }

    #[inline]
    unsafe fn mutable_to(self, ptr: *mut T) -> RefMut<'obj, T> {
        RefMut::new(ptr, self.lazy_handle)
    }

    #[inline]
    pub unsafe fn mutable(self) -> RefMut<'obj, T> {
        let ptr = self.ptr as *mut T;
        self.mutable_to(ptr)
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    pub fn owned<'b>(&self) -> Ref<'b, T> {
        Ref::from_handle(self.handle().clone(), self.ptr)
    }

    pub fn from_handle(handle: ObjectHandle, ptr: *const T) -> Self {
        Self::new(ptr, LazyHandle::new_owned(handle))
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

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'a, T> From<Ref<'a, T>> for GlobalPtr<T> {
    fn from(value: Ref<'a, T>) -> Self {
        GlobalPtr::new(value.handle().id(), value.offset())
    }
}

pub struct RefMut<'obj, T> {
    ptr: *mut T,
    lazy_handle: LazyHandle<'obj>,
    _pd: PhantomData<&'obj mut T>,
}

impl<'obj, T> RefMut<'obj, T> {
    fn new(ptr: *mut T, lazy_handle: LazyHandle<'obj>) -> Self {
        Self {
            ptr,
            lazy_handle,
            _pd: PhantomData,
        }
    }

    #[inline]
    pub fn raw(&self) -> *mut T {
        self.ptr
    }

    pub unsafe fn from_raw_parts(ptr: *mut T, handle: &'obj ObjectHandle) -> Self {
        Self::new(ptr, LazyHandle::new_borrowed(handle))
    }

    pub fn from_handle(handle: ObjectHandle, ptr: *mut T) -> Self {
        Self::new(ptr, LazyHandle::new_owned(handle))
    }

    #[inline]
    pub unsafe fn from_ptr(ptr: *mut T) -> Self {
        Self::new(ptr, LazyHandle::default())
    }

    #[inline]
    pub unsafe fn cast<U>(self) -> RefMut<'obj, U> {
        RefMut::new(self.ptr.cast(), self.lazy_handle)
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.lazy_handle.handle(self.ptr.cast())
    }

    pub fn offset(&self) -> u64 {
        self.handle().ptr_local(self.ptr.cast()).unwrap() as u64
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::new(self.handle().id(), self.offset())
    }

    // Note: takes ownership to avoid aliasing
    pub fn owned<'b>(self) -> RefMut<'b, T> {
        RefMut::from_handle(self.handle().clone(), self.ptr)
    }
}

impl<'obj, T: core::fmt::Debug> core::fmt::Debug for RefMut<'obj, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.deref())
    }
}

impl<'obj, T> Deref for RefMut<'obj, T> {
    type Target = T;

    #[inline]
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

fn range_bounds_to_start_and_end(len: usize, range: impl RangeBounds<usize>) -> (usize, usize) {
    let start = match range.start_bound() {
        std::ops::Bound::Included(n) => *n,
        std::ops::Bound::Excluded(n) => n.saturating_add(1),
        std::ops::Bound::Unbounded => 0,
    };
    let end = match range.start_bound() {
        std::ops::Bound::Included(n) => n.saturating_add(1),
        std::ops::Bound::Excluded(n) => *n,
        std::ops::Bound::Unbounded => len,
    };
    (start, end)
}

pub struct RefSlice<'a, T> {
    ptr: Ref<'a, T>,
    len: usize,
}

impl<'a, T> RefSlice<'a, T> {
    #[inline]
    pub unsafe fn from_ref(ptr: Ref<'a, T>, len: usize) -> Self {
        Self { ptr, len }
    }

    #[inline]
    pub fn as_slice(&self) -> &'a [T] {
        unsafe { core::slice::from_raw_parts(self.ptr.raw(), self.len) }
    }

    #[inline]
    pub fn slice(self, range: impl RangeBounds<usize>) -> Self {
        let (start, end) = range_bounds_to_start_and_end(self.len, range);
        let len = end - start;
        if let Some(r) = self.get_ref(start) {
            unsafe { Self::from_ref(r, len) }
        } else {
            unsafe { Self::from_ref(self.ptr, 0) }
        }
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_ptr(ptr) })
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let ptr = self.as_slice().get(idx)?;
        Some(ptr)
    }

    #[inline]
    pub fn get_into(self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)? as *const T;
        Some(Ref::new(ptr, self.ptr.lazy_handle))
    }

    #[inline]
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

    pub fn as_slice(&self) -> &'a [T] {
        unsafe { core::slice::from_raw_parts(self.ptr.raw(), self.len) }
    }

    pub fn as_slice_mut(&mut self) -> &'a mut [T] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr.raw(), self.len) }
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'a, T>> {
        let ptr = self.as_slice().get(idx)?;
        Some(unsafe { Ref::from_ptr(ptr) })
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let ptr = self.as_slice().get(idx)?;
        Some(ptr)
    }

    pub fn get_mut(&mut self, idx: usize) -> Option<RefMut<'a, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)?;
        Some(unsafe { RefMut::from_ptr(ptr) })
    }

    #[inline]
    pub fn get_into(mut self, idx: usize) -> Option<RefMut<'a, T>> {
        let ptr = self.as_slice_mut().get_mut(idx)? as *mut T;
        Some(RefMut::new(ptr, self.ptr.lazy_handle))
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
