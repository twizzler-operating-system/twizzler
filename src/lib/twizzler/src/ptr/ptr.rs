use std::{
    intrinsics::{likely, unlikely},
    marker::{PhantomData, PhantomPinned},
    mem::size_of,
    ptr::{addr_of, addr_of_mut},
};

use twizzler_abi::object::{make_invariant_pointer, split_invariant_pointer};
use twizzler_runtime_api::FotResolveError;

use super::{GlobalPtr, InvPtrBuilder, ResolvedMutPtr, ResolvedPtr};
use crate::{
    marker::{InPlace, Invariant, InvariantValue, StoreEffect, TryStoreEffect},
    object::fot::FotEntry,
    tx::TxResult,
};

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
    pub const fn null() -> Self {
        Self {
            bits: 0,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    // TODO: these maybe are safe
    pub const unsafe fn new(bits: u64) -> Self {
        Self {
            bits,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    // TODO: these maybe are safe
    pub const unsafe fn from_raw_parts(fot_idx: usize, offset: u64) -> Self {
        Self {
            bits: make_invariant_pointer(fot_idx, offset),
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub const fn is_null(&self) -> bool {
        self.bits == 0
    }

    pub const fn raw(&self) -> u64 {
        self.bits
    }

    pub const fn is_local(&self) -> bool {
        split_invariant_pointer(self.raw()).0 == 0
    }

    pub fn set(&mut self, dest: impl Into<InvPtrBuilder<T>>) -> TxResult<()> {
        let raw_self = addr_of_mut!(*self);
        let (handle, _) = twizzler_runtime_api::get_runtime()
            .ptr_to_handle(raw_self as *const u8)
            .unwrap();
        let mut in_place = InPlace::new(&handle);
        let value = Self::store(dest.into(), &mut in_place);

        // TODO: do we need to drop anything?

        *self = value;
        Ok(())
    }

    pub unsafe fn resolve(&self) -> ResolvedPtr<'_, T> {
        self.try_resolve().unwrap()
    }

    /// Resolves an invariant pointer.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub unsafe fn try_resolve(&self) -> Result<ResolvedPtr<'_, T>, FotResolveError> {
        if unlikely(self.is_null()) {
            return Err(FotResolveError::NullPointer);
        }
        // Find the address of our invariant pointer, to locate the object it resides in.
        let this = addr_of!(*self) as *const u8;
        // Split the pointer, and grab the offset as a usize.
        let (fote, off) = split_invariant_pointer(self.raw());
        let offset = off as usize;
        let valid_len = offset + size_of::<T>();
        // If we're doing a local transform, let's just get the start and calculate an offset.
        if likely(fote == 0) {
            // TODO: cache this?.
            let (start, _) = twizzler_runtime_api::get_runtime()
                .ptr_to_object_start(this, valid_len)
                .ok_or(FotResolveError::InvalidArgument)?;
            // Safety: we ensure we point to valid memory by ensuring contiguous length from start
            // to our offset + size of T, above.
            return unsafe { Ok(ResolvedPtr::new(start.add(offset) as *const T)) };
        }

        // We need to consult the FOT, so ask the runtime.
        let runtime = twizzler_runtime_api::get_runtime();
        // TODO: cache this.
        let (our_handle, _) = runtime
            .ptr_to_handle(this)
            .ok_or(FotResolveError::InvalidArgument)?;
        let start = twizzler_runtime_api::get_runtime().resolve_fot_to_object_start(
            &our_handle,
            fote,
            valid_len,
        )?;
        // Safety: we ensure we point to valid memory by ensuring contiguous length from start
        // to our offset + size of T, above.
        match start {
            twizzler_runtime_api::StartOrHandle::Start(start) => unsafe {
                Ok(ResolvedPtr::new(start.add(offset) as *const T))
            },
            twizzler_runtime_api::StartOrHandle::Handle(handle) => unsafe {
                Ok(ResolvedPtr::new_with_handle(
                    handle.start.add(offset) as *const T,
                    handle,
                ))
            },
        }
    }

    pub fn try_as_global(&self) -> Result<GlobalPtr<T>, FotResolveError> {
        let resolved = unsafe { self.try_resolve() }?;
        Ok(unsafe { GlobalPtr::new(resolved.handle().id, split_invariant_pointer(self.raw()).1) })
    }
}

unsafe impl<T> InvariantValue for InvPtr<T> {}
unsafe impl<T> Invariant for InvPtr<T> {}

impl<T> TryStoreEffect for InvPtr<T> {
    type MoveCtor = InvPtrBuilder<T>;
    type Error = ();

    fn try_store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut crate::marker::InPlace<'a>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(if ctor.is_local() {
            unsafe { Self::new(ctor.offset()) }
        } else {
            let runtime = twizzler_runtime_api::get_runtime();
            let (fot, idx) = runtime.add_fot_entry(&in_place.handle()).ok_or(())?;
            let fot = fot as *mut FotEntry;

            unsafe {
                fot.write(ctor.fot_entry());
                Self::from_raw_parts(idx, ctor.offset())
            }
        })
    }
}

impl<T> StoreEffect for InvPtr<T> {
    type MoveCtor = InvPtrBuilder<T>;

    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut crate::marker::InPlace<'a>) -> Self
    where
        Self: Sized,
    {
        <Self as TryStoreEffect>::try_store(ctor, in_place).unwrap()
    }
}
