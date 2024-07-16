use std::marker::{PhantomData, PhantomPinned};

use super::{GlobalPtr, InvPtrBuilder, ResolvedPtr};

// TODO: niche optimization -- sizeof Option<InvPtr<T>> == 8 -- null => None.
#[repr(transparent)]
pub struct InvPtr<T> {
    bits: u64,
    _pd: PhantomData<*const T>,
    _pp: PhantomPinned,
}

// Safety: These are the standard library rules for references (https://doc.rust-lang.org/std/primitive.reference.html).
unsafe impl<T: Sync> Sync for InvPtr<T> {}
unsafe impl<T: Sync> Send for InvPtr<T> {}

impl<T> InvPtr<T> {
    pub fn null() -> Self {
        Self {
            bits: 0,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub fn set(&mut self, _builder: impl Into<InvPtrBuilder<T>>) {
        todo!()
    }

    pub fn raw(&self) -> u64 {
        self.bits
    }

    pub fn is_local(&self) -> bool {
        todo!()
    }

    /// Resolves an invariant pointer.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve(&self) -> Result<ResolvedPtr<'_, T>, ()> {
        todo!()
    }

    pub fn as_global(&self) -> Result<GlobalPtr<T>, ()> {
        todo!()
    }
}
