//! Marker traits used to indicate safety for storing data in objects and using a struct as a base
//! type.

use core::{
    cell::UnsafeCell,
    sync::atomic::{
        AtomicBool, AtomicI16, AtomicI32, AtomicI64, AtomicI8, AtomicIsize, AtomicU16, AtomicU32,
        AtomicU64, AtomicU8, AtomicUsize,
    },
};

/// This auto trait specifies that some type is "safe" to to store inside an object. This means that
/// the type contains no non-invariant references nor any unsafe interior mutability not implemented
/// via twizzler-nando.
///
/// # Safety
/// Manually marking type as safe requires that the programmer adhere to the rules set above.

pub unsafe auto trait ObjSafe {}

impl<T> !ObjSafe for *const T {}
impl<T> !ObjSafe for *mut T {}
impl<T> !ObjSafe for &T {}
impl<T> !ObjSafe for &mut T {}
impl<T> !ObjSafe for UnsafeCell<T> {}
unsafe impl ObjSafe for AtomicBool {}
unsafe impl ObjSafe for AtomicU16 {}
unsafe impl ObjSafe for AtomicU32 {}
unsafe impl ObjSafe for AtomicU64 {}
unsafe impl ObjSafe for AtomicU8 {}
unsafe impl ObjSafe for AtomicUsize {}
unsafe impl ObjSafe for AtomicI16 {}
unsafe impl ObjSafe for AtomicI32 {}
unsafe impl ObjSafe for AtomicI64 {}
unsafe impl ObjSafe for AtomicI8 {}
unsafe impl ObjSafe for AtomicIsize {}
