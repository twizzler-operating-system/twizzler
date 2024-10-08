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
#[rustc_on_unimplemented(
    message = "`{Self}` is not safe to be stored in an object",
    label = "`{Self}` is not safe to be stored in an object"
)]
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

/// Version for a base type.
#[derive(Clone, Copy, Debug)]
pub struct BaseVersion {}
/// Tag for a base type. Each base type must have a unique tag.
#[derive(Clone, Copy, Debug)]
pub struct BaseTag {}
/// Trait that all base types must implement.
#[rustc_on_unimplemented(
    message = "`{Self}` is not safe to be a base type for an object",
    label = "`{Self}` is not safe to be a base type for an object"
)]
pub trait BaseType {
    /// Construct a new base type.
    fn init<T>(_t: T) -> Self;
    /// Returns a list of valid tags and versions for this type.
    fn tags() -> &'static [(BaseVersion, BaseTag)];
}

/*
impl<T: Default + ObjSafe> BaseType for T {
    default fn init<P>(_: P) -> T {
        T::default()
    }
}

impl<T: Default + ObjSafe> BaseType for &[T] {
    default fn init<P>(_: P) -> Self {
        <&[T]>::default()
    }
}
*/
