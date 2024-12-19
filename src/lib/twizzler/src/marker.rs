//! Marker types for invariance, store side-effects, and base types.

use std::mem::MaybeUninit;

/// Indicates that a type is _invariant_ and thus can be stored in an object.
///
/// # Safety
/// The implementation must ensure that the type is invariant, meaning that the type must:
///   - Be FFI safe.
///   - Be stable in-memory (independent of architecture). This means, among other things, that the
///     type must be fixed-width. For example, usize is not `Invariant`.
pub unsafe trait Invariant {}

unsafe impl Invariant for u8 {}
unsafe impl Invariant for u16 {}
unsafe impl Invariant for u32 {}
unsafe impl Invariant for u64 {}
unsafe impl Invariant for bool {}
unsafe impl Invariant for i8 {}
unsafe impl Invariant for i16 {}
unsafe impl Invariant for i32 {}
unsafe impl Invariant for i64 {}

unsafe impl Invariant for f64 {}
unsafe impl Invariant for f32 {}

unsafe impl Invariant for () {}

unsafe impl<T: Invariant, const N: usize> Invariant for [T; N] {}

unsafe impl<T: Invariant> Invariant for (T,) {}

unsafe impl<T: Invariant> Invariant for Option<T> {}
unsafe impl<R: Invariant, E: Invariant> Invariant for Result<R, E> {}

/// The type may move between objects without side effects. Notably, this is
/// not implemented for invariant pointers or types that contain them, since an invariant pointer
/// may reference an object's Foreign Object Table. This is a little restrictive (technically
/// intra-object pointers are safe to move intra-object), but it's the best we can do at
/// compile-time.
///
/// # Safety
/// The implementation must ensure that no store side effects must occur when writing this value to
/// object memory.
pub unsafe auto trait StoreCopy {}

/// A zero-sized phantom marker for indicating that the containing type has a side effect when
/// storing (e.g. it has an invariant pointer).
#[derive(Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Debug)]
pub struct PhantomStoreEffect;

impl !StoreCopy for PhantomStoreEffect {}
impl !Unpin for PhantomStoreEffect {}

#[derive(Debug)]
pub struct Storable<T>(MaybeUninit<T>, Option<()>);

unsafe impl<T> StoreCopy for Storable<T> {}

impl<T: StoreCopy> From<T> for Storable<T> {
    fn from(value: T) -> Self {
        Self(MaybeUninit::new(value), None)
    }
}

impl<T: StoreCopy> Storable<T> {
    pub fn into_inner(self) -> T {
        unsafe { self.0.assume_init() }
    }
}

impl<T> Storable<T> {
    pub unsafe fn new(value: T) -> Self {
        Self(MaybeUninit::new(value), None)
    }

    pub unsafe fn into_inner_unchecked(self) -> T {
        unsafe { self.0.assume_init() }
    }
}

pub trait BaseType {
    /// The fingerprint of this type.
    fn fingerprint() -> u64 {
        0
    }
}

impl BaseType for u8 {}
impl BaseType for u16 {}
impl BaseType for u32 {}
impl BaseType for u64 {}
