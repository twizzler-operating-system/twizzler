use std::{
    intrinsics::{likely, size_of, unlikely},
    marker::{PhantomData, PhantomPinned},
    ptr::{addr_of, addr_of_mut},
};

use twizzler_abi::object::{
    make_invariant_pointer, split_invariant_pointer, MAX_SIZE, NULLPAGE_SIZE,
};
use twizzler_runtime_api::FotResolveError;

use super::{GlobalPtr, InvPtrBuilder, ResolvedMutPtr, ResolvedPtr};
use crate::{
    marker::{InPlace, Invariant, InvariantValue, StoreEffect, TryStoreEffect},
    object::fot::{FotEntry, FotResolve},
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
    #[inline]
    pub const fn null() -> Self {
        Self {
            bits: 0,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    // TODO: these maybe are safe
    #[inline]
    pub const unsafe fn new(bits: u64) -> Self {
        Self {
            bits,
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    // TODO: these maybe are safe
    #[inline]
    pub const unsafe fn from_raw_parts(fot_idx: usize, offset: u64) -> Self {
        Self {
            bits: make_invariant_pointer(fot_idx, offset),
            _pd: PhantomData,
            _pp: PhantomPinned,
        }
    }

    #[inline]
    pub const fn is_null(&self) -> bool {
        self.bits == 0
    }

    #[inline]
    pub const fn raw(&self) -> u64 {
        self.bits
    }

    #[inline]
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

    #[inline]
    fn this(&self) -> *const u8 {
        // Find the address of our invariant pointer, to locate the object it resides in.
        addr_of!(*self).cast()
    }

    #[inline]
    pub unsafe fn resolve(&self) -> ResolvedPtr<'_, T> {
        let (fote, off) = split_invariant_pointer(self.raw());
        // If we're doing a local transform, let's just get the start and calculate an offset.
        if likely(fote == 0) {
            let result = unsafe { Self::resolve_fast_local(self.this(), off as usize) };
            return result;
        }

        /*
        let cached = resolve_thread_local_cache(self.this());
        if likely(cached.is_some()) {
            return ResolvedPtr::new(cached.unwrap_unchecked().cast());
        }
        */
        let result = self.try_resolve().unwrap();

        //insert_thread_local_cache(self.this(), result.ptr().cast());
        return result;
    }

    #[inline]
    unsafe fn resolve_fast_local<'a>(this: *const u8, offset: usize) -> ResolvedPtr<'a, T> {
        let slot = (this as usize) / MAX_SIZE;
        let start = (slot * MAX_SIZE) as *const u8;
        ResolvedPtr::new(start.add(offset).cast())
    }

    /// Resolves an invariant pointer.
    ///
    /// Note that this function needs to ask the runtime for help, since it does not know which
    /// object to use for FOT translation. If you know that an invariant pointer resides in an
    /// object, you can use [Object::resolve].
    #[inline]
    pub unsafe fn try_resolve(&self) -> Result<ResolvedPtr<'_, T>, FotResolveError> {
        // Split the pointer, and grab the offset as a usize.
        let (fote, off) = split_invariant_pointer(self.raw());
        let offset = off as usize;
        let valid_len = offset + size_of::<T>();

        if unlikely(self.is_null()) {
            return Err(FotResolveError::NullPointer);
        }

        if unlikely(offset + valid_len > MAX_SIZE - NULLPAGE_SIZE) {
            return Err(FotResolveError::InvalidArgument);
        }

        // If we're doing a local transform, let's just get the start and calculate an offset.
        if likely(fote == 0) {
            unsafe { return Ok(Self::resolve_fast_local(self.this(), offset)) }
        }

        // We need to consult the FOT, so ask the runtime.
        let runtime = twizzler_runtime_api::get_runtime();
        let our_start = runtime
            .ptr_to_object_start(self.this(), valid_len)
            .ok_or(FotResolveError::InvalidArgument)?;
        let start = twizzler_runtime_api::get_runtime().resolve_fot_to_object_start(
            twizzler_runtime_api::StartOrHandleRef::Start(our_start.0),
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

const MAX_SLOTS: usize = 8;
#[derive(Copy, Clone, Debug)]
struct Slot {
    ptr: *const u8,
    result: *const u8,
}

impl Slot {
    const fn null() -> Self {
        Self {
            ptr: std::ptr::null(),
            result: std::ptr::null(),
        }
    }
}
#[thread_local]
static mut SLOTS: [Slot; MAX_SLOTS] = [Slot::null(); MAX_SLOTS];

#[inline]
fn resolve_thread_local_cache(this: *const u8) -> Option<*const u8> {
    let slots = unsafe { &mut SLOTS };
    for i in 0..MAX_SLOTS {
        if slots[i].ptr == this {
            let result = slots[i].result;
            if i > MAX_SLOTS / 2 {
                slots.swap(MAX_SLOTS / 2, i);
            }
            return Some(result);
        }
    }
    None
}

#[inline]
fn insert_thread_local_cache(this: *const u8, result: *const u8) {
    let slots = unsafe { &mut SLOTS };
    for slot in slots.iter_mut() {
        if slot.ptr.is_null() {
            slot.ptr = this;
            slot.result = result;
            return;
        }
    }

    slots.copy_within(0..(MAX_SLOTS - 1), 1);
    slots[0].ptr = this;
    slots[0].result = result;
}

fn clear_thread_local_cache(start: usize, end: usize) {
    let slots = unsafe { &mut SLOTS };
    for slot in slots {
        if slot.ptr.addr() >= start && slot.ptr.addr() < end {
            slot.ptr = std::ptr::null();
        }
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
        Ok(if ctor.is_local() || ctor.id() == in_place.handle().id {
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

mod tests {
    use crate::{
        object::{BaseType, InitializedObject, ObjectBuilder},
        ptr::InvPtr,
    };

    #[derive(crate::Invariant)]
    struct Foo {
        ptr: InvPtr<Bar>,
    }
    impl BaseType for Foo {}

    #[derive(crate::Invariant)]
    struct Bar {
        x: u32,
    }
    impl BaseType for Bar {}
    extern crate test;
    #[bench]
    fn bench_ptr_resolve_local(bench: &mut test::Bencher) {
        let obj = ObjectBuilder::default()
            .init(Foo {
                ptr: unsafe { InvPtr::new(0x4000) },
            })
            .unwrap();
        let base = unsafe { obj.base_mut() };
        assert!(base.ptr.is_local());
        bench.iter(|| {
            this_is_the_test(base);
        });
    }

    #[bench]
    fn bench_ptr_resolve_fote(bench: &mut test::Bencher) {
        let bar = ObjectBuilder::default().init(Bar { x: 3 }).unwrap();
        let obj = ObjectBuilder::default()
            .construct(|ci| Foo {
                ptr: ci.in_place().store(bar.base()),
            })
            .unwrap();
        let base = unsafe { obj.base_mut() };
        assert!(!base.ptr.is_local());
        bench.iter(|| {
            this_is_the_test(base);
        });
    }

    #[no_mangle]
    #[inline(never)]
    fn this_is_the_test(foo: &mut Foo) {
        for i in 0..1000 {
            //let foo = core::hint::black_box(&mut *foo);
            let _res = unsafe { foo.ptr.resolve() };
            core::hint::black_box(_res);
        }
    }
}
