use std::alloc::Layout;

use twizzler_abi::object::ObjID;

use super::Allocator;
use crate::{
    marker::Invariant,
    ptr::{GlobalPtr, Ref},
};

/// A global pointer that owns the memory it points to, and will free it on drop.
pub struct OwnedGlobalPtr<T, A: Allocator> {
    global: GlobalPtr<T>,
    alloc: A,
}

unsafe impl<T: Invariant, A: Allocator> Invariant for OwnedGlobalPtr<T, A> {}

impl<T, A: Allocator> Drop for OwnedGlobalPtr<T, A> {
    fn drop(&mut self) {
        let layout = Layout::new::<T>();
        unsafe { self.alloc.dealloc(self.global().cast(), layout) };
    }
}

impl<T, A: Allocator> OwnedGlobalPtr<T, A> {
    pub fn global(&self) -> GlobalPtr<T> {
        self.global
    }

    pub unsafe fn from_global(global: GlobalPtr<T>, alloc: A) -> Self {
        Self { global, alloc }
    }

    pub fn resolve<'a>(&'a self) -> Ref<'a, T> {
        unsafe { self.global.resolve() }
    }

    pub fn allocator(&self) -> &A {
        &self.alloc
    }

    /// Returns the object ID of the global pointer.
    pub fn id(&self) -> ObjID {
        self.global().id()
    }

    /// Returns the offset of the global pointer.
    pub fn offset(&self) -> u64 {
        self.global().offset()
    }
}
