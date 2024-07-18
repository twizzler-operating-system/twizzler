use std::{
    intrinsics::{likely, unlikely},
    marker::{PhantomData, PhantomPinned},
    mem::size_of,
};

use twizzler_abi::object::{make_invariant_pointer, split_invariant_pointer};
use twizzler_runtime_api::FotResolveError;

use super::{GlobalPtr, InvPtrBuilder, ResolvedPtr};
use crate::{
    marker::{Invariant, InvariantValue, StoreEffect, TryStoreEffect},
    object::{fot::FotEntry, InitializedObject},
    tx::{TxError, TxResult},
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
    pub fn null() -> Self {
        Self {
            bits: 0,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub unsafe fn new(bits: u64) -> Self {
        Self {
            bits,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub unsafe fn from_raw_parts(fot_idx: usize, offset: u64) -> Self {
        Self {
            bits: make_invariant_pointer(fot_idx, offset),
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    pub fn is_null(&self) -> bool {
        self.bits == 0
    }

    pub fn raw(&self) -> u64 {
        self.bits
    }

    pub fn is_local(&self) -> bool {
        split_invariant_pointer(self.raw()).0 == 0
    }

    /// Resolves an invariant pointer.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    pub fn resolve(&self) -> Result<ResolvedPtr<'_, T>, FotResolveError> {
        if unlikely(self.is_null()) {
            return Err(FotResolveError::NullPointer);
        }
        // Find the address of our invariant pointer, to locate the object it resides in.
        let this = self as *const _ as *const u8;
        // Split the pointer, and grab the offset as a usize.
        let (fote, off) = split_invariant_pointer(self.raw());
        let offset = off as usize;
        let valid_len = offset + size_of::<T>();
        // If we're doing a local transform, let's just get the start and calculate an offset.
        if likely(fote == 0) {
            // TODO: cache this?.
            let start = twizzler_runtime_api::get_runtime()
                .ptr_to_object_start(this, valid_len)
                .ok_or(FotResolveError::InvalidArgument)?;
            // Safety: we ensure we point to valid memory by ensuring contiguous length from start
            // to our offset + size of T, above.
            return unsafe { Ok(ResolvedPtr::new(start.add(offset) as *const T)) };
        }

        // We need to consult the FOT, so ask the runtime.
        let runtime = twizzler_runtime_api::get_runtime();
        // TODO: cache this.
        let our_handle = runtime
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
                Ok(ResolvedPtr::new(handle.start.add(offset) as *const T))
            },
        }
    }

    pub fn as_global(&self) -> Result<GlobalPtr<T>, FotResolveError> {
        let resolved = self.resolve()?;
        Ok(GlobalPtr::new(
            resolved.handle().id,
            split_invariant_pointer(self.raw()).1,
        ))
    }
}

unsafe impl<T> InvariantValue for InvPtr<T> {}
unsafe impl<T> Invariant for InvPtr<T> {}

impl<T: Invariant> TryStoreEffect for InvPtr<T> {
    type MoveCtor = InvPtrBuilder<T>;
    type Error = ();

    fn try_store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut crate::marker::InPlace<'a, Self>,
        tx: impl crate::tx::TxHandle<'a>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(if ctor.is_local() {
            unsafe { Self::new(ctor.offset()) }
        } else {
            let runtime = twizzler_runtime_api::get_runtime();
            let handle = runtime
                .ptr_to_handle(in_place.place() as *const _ as *const u8)
                .ok_or(())?;
            let (fot, idx) = runtime.add_fot_entry(&handle).ok_or(())?;
            let fot = fot as *mut FotEntry;

            unsafe {
                fot.write(ctor.fot_entry());
                Self::from_raw_parts(idx, ctor.offset())
            }
        })
    }
}

impl<T: Invariant> StoreEffect for InvPtr<T> {
    type MoveCtor = InvPtrBuilder<T>;

    fn store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut crate::marker::InPlace<'a, Self>,
        tx: impl crate::tx::TxHandle<'a>,
    ) -> Self
    where
        Self: Sized,
    {
        <Self as TryStoreEffect>::try_store(ctor, in_place, tx).unwrap()
    }
}
