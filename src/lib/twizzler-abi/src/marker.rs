use core::{
    cell::UnsafeCell,
    sync::atomic::{
        AtomicBool, AtomicI16, AtomicI32, AtomicI64, AtomicI8, AtomicIsize, AtomicU16, AtomicU32,
        AtomicU64, AtomicU8, AtomicUsize,
    },
};

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

#[derive(Debug)]
pub struct BaseVersion {}
#[derive(Debug)]
pub struct BaseTag {}
#[rustc_on_unimplemented(
    message = "`{Self}` is not safe to be a base type for an object",
    label = "`{Self}` is not safe to be a base type for an object"
)]
pub trait BaseType {
    fn init<T>(_t: T) -> Self;
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
