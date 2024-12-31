use super::Allocator;
use crate::ptr::{GlobalPtr, Ref};

pub struct OwnedGlobalPtr<T, A: Allocator> {
    global: GlobalPtr<T>,
    alloc: A,
}

impl<T, A: Allocator> Drop for OwnedGlobalPtr<T, A> {
    fn drop(&mut self) {
        todo!()
    }
}

impl<T, A: Allocator> OwnedGlobalPtr<T, A> {
    pub fn global(&self) -> GlobalPtr<T> {
        self.global
    }

    pub unsafe fn from_global(global: GlobalPtr<T>, alloc: A) -> Self {
        Self { global, alloc }
    }

    pub fn resolve(&self) -> Ref<'_, T> {
        todo!()
    }
}
