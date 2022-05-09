#[rustc_on_unimplemented(
    message = "`{Self}` is not safe to be stored in an object",
    label = "`{Self}` is not safe to be stored in an object"
)]
pub auto trait ObjSafe {}

impl<T> !ObjSafe for *const T {}

#[rustc_on_unimplemented(
    message = "`{Self}` is not safe to be a base type for an object",
    label = "`{Self}` is not safe to be a base type for an object"
)]
pub trait BaseType {
    fn init<T>(_t: T) -> Self;
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
