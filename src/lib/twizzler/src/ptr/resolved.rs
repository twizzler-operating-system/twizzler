use std::{
    borrow::Cow,
    cell::OnceCell,
    mem::MaybeUninit,
    ops::{Deref, DerefMut},
    pin::Pin,
};

use twizzler_runtime_api::ObjectHandle;

use super::{GlobalPtr, InvPtrBuilder};
use crate::marker::{CopyStorable, StorePlace};

#[repr(transparent)]
#[derive(Clone, Default)]
pub(crate) struct OnceHandle<'a>(OnceCell<Cow<'a, ObjectHandle>>);

impl<'a> OnceHandle<'a> {
    pub(crate) fn handle(&self, ptr: *const u8) -> &ObjectHandle {
        self.0.get_or_init(|| {
            let runtime = twizzler_runtime_api::get_runtime();
            Cow::Owned(runtime.ptr_to_handle(ptr).unwrap().0)
        })
    }

    pub(crate) fn new(handle: ObjectHandle) -> Self {
        Self(OnceCell::from(Cow::Owned(handle)))
    }

    pub(crate) fn new_ref(handle: &'a ObjectHandle) -> Self {
        Self(OnceCell::from(Cow::Borrowed(handle)))
    }
}

#[derive(Clone)]
pub struct ResolvedPtr<'obj, T> {
    ptr: *const T,
    once_handle: OnceHandle<'obj>,
}

impl<'obj, T> ResolvedPtr<'obj, T> {
    pub(crate) unsafe fn new(ptr: *const T) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::default(),
        }
    }

    pub(crate) unsafe fn new_with_handle(ptr: *const T, handle: ObjectHandle) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::new(handle),
        }
    }

    pub(crate) unsafe fn new_with_handle_ref(ptr: *const T, handle: &'obj ObjectHandle) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::new_ref(handle),
        }
    }

    pub unsafe fn into_mut(self) -> ResolvedMutPtr<'obj, T> {
        let ResolvedPtr { ptr, once_handle } = self;
        ResolvedMutPtr {
            once_handle,
            ptr: ptr as *mut T,
        }
    }

    pub unsafe fn as_mut(&mut self) -> &mut T {
        self.ptr.cast_mut().as_mut().unwrap()
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.once_handle.handle(self.ptr as *const u8)
    }

    pub fn ptr(&self) -> *const T {
        self.ptr
    }

    pub fn owned<'a>(&self) -> ResolvedPtr<'a, T> {
        unsafe { ResolvedPtr::new_with_handle(self.ptr, self.handle().clone()) }
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::from_va(self.ptr()).unwrap()
    }

    pub fn inv_ptr(&self) -> InvPtrBuilder<T> {
        InvPtrBuilder::from_global(self.global())
    }
}

impl<'obj, T> Deref for ResolvedPtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

pub struct ResolvedMutPtr<'obj, T> {
    once_handle: OnceHandle<'obj>,
    ptr: *mut T,
}

impl<'obj, T> ResolvedMutPtr<'obj, T> {
    pub(crate) unsafe fn new(ptr: *mut T) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::default(),
        }
    }

    pub(crate) unsafe fn new_with_handle(ptr: *mut T, handle: ObjectHandle) -> Self {
        Self {
            ptr,
            once_handle: OnceHandle::new(handle),
        }
    }

    pub fn handle(&self) -> &ObjectHandle {
        self.once_handle.handle(self.ptr as *const u8)
    }

    pub fn ptr(&self) -> *mut T {
        self.ptr
    }

    pub fn owned<'a>(self) -> ResolvedMutPtr<'a, T> {
        unsafe { ResolvedMutPtr::new_with_handle(self.ptr, self.handle().clone()) }
    }

    pub fn global(&self) -> GlobalPtr<T> {
        GlobalPtr::from_va(self.ptr()).unwrap()
    }

    pub fn set_with(&mut self, ctor: impl FnOnce(StorePlace) -> T) {
        todo!()
    }

    pub unsafe fn as_mut(&mut self) -> &mut T {
        self.ptr.as_mut().unwrap()
    }

    pub fn as_pin(&mut self) -> Pin<&mut T> {
        unsafe { Pin::new_unchecked(self.ptr.as_mut().unwrap()) }
    }

    pub fn inv_ptr(&self) -> InvPtrBuilder<T> {
        InvPtrBuilder::from_global(self.global())
    }
}

impl<'obj, T> ResolvedMutPtr<'obj, MaybeUninit<T>> {
    pub fn write(self, item: T) -> ResolvedMutPtr<'obj, T> {
        let ResolvedMutPtr { once_handle, ptr } = self;
        unsafe {
            (*ptr).write(item);
        }

        ResolvedMutPtr {
            once_handle,
            ptr: ptr as *mut T,
        }
    }
}

impl<'obj, T> Deref for ResolvedMutPtr<'obj, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_ref().unwrap_unchecked() }
    }
}

impl<'obj, T: CopyStorable> DerefMut for ResolvedMutPtr<'obj, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Safety: we are pointing to a mutable object, that we have locked.
        unsafe { self.ptr.as_mut().unwrap_unchecked() }
    }
}

impl<'a, T> From<ResolvedMutPtr<'a, T>> for ResolvedPtr<'a, T> {
    fn from(value: ResolvedMutPtr<'a, T>) -> Self {
        Self {
            ptr: value.ptr,
            once_handle: value.once_handle,
        }
    }
}

impl<'a, T> From<ResolvedPtr<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ResolvedPtr<'a, T>) -> Self {
        InvPtrBuilder::from_global(value.global())
    }
}

impl<'a, T> From<ResolvedMutPtr<'a, T>> for InvPtrBuilder<T> {
    fn from(value: ResolvedMutPtr<'a, T>) -> Self {
        InvPtrBuilder::from_global(value.global())
    }
}

impl<'a, T> From<&ResolvedPtr<'a, T>> for InvPtrBuilder<T> {
    fn from(value: &ResolvedPtr<'a, T>) -> Self {
        InvPtrBuilder::from_global(value.global())
    }
}

impl<'a, T> From<&ResolvedMutPtr<'a, T>> for InvPtrBuilder<T> {
    fn from(value: &ResolvedMutPtr<'a, T>) -> Self {
        InvPtrBuilder::from_global(value.global())
    }
}
