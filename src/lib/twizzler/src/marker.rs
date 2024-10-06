use std::{convert::Infallible, mem::MaybeUninit};

use twizzler_runtime_api::ObjectHandle;

pub use crate::object::BaseType;

pub unsafe auto trait InvariantValue {}

impl<T> !InvariantValue for *const T {}
impl<T> !InvariantValue for *mut T {}
impl<T> !InvariantValue for &T {}
impl<T> !InvariantValue for &mut T {}

pub unsafe trait Invariant: InvariantValue {}

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

pub unsafe auto trait CopyStorable {}

pub struct PhantomStoreEffect;

impl !CopyStorable for PhantomStoreEffect {}
impl !Unpin for PhantomStoreEffect {}

#[repr(transparent)]
pub struct Storer<T>(T);

impl<T> Storer<T> {
    pub unsafe fn new_move(value: T) -> Self {
        Self(value)
    }

    pub fn store(value: impl Into<T::MoveCtor>, sp: &mut StorePlace) -> Self
    where
        T: StoreEffect,
    {
        Self(sp.store(value))
    }

    pub fn into_inner(self) -> T {
        self.0
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Storer<U> {
        unsafe { Storer::new_move(f(self.into_inner())) }
    }
}

impl<T: CopyStorable> Storer<T> {
    pub fn new(value: T) -> Self {
        Self(value)
    }
}

//unsafe impl<T> CopyStorable for Storer<T> {}

/*
pub trait Storable<T>: CopyStorable {
    type Out;
    fn storable(self) -> T
    where
        Self: Sized;
}

impl<T: CopyStorable> Storable<T> for T {
    type Out = T;
    fn storable(self) -> T
    where
        Self: Sized,
    {
        self
    }
}

impl<T> Storable<T> for Storer<T> {
    type Out = T;
    fn storable(self) -> T
    where
        Self: Sized,
    {
        self.into_inner()
    }
}
*/

pub struct StorePlace<'a> {
    handle: &'a ObjectHandle,
}

impl<'a> StorePlace<'a> {
    pub(crate) fn new(handle: &'a ObjectHandle) -> Self {
        Self { handle }
    }

    pub fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<'a> StorePlace<'a> {
    pub fn store<V: StoreEffect>(&mut self, item: impl Into<V::MoveCtor>) -> V {
        V::store(item.into(), self)
    }
}

impl<'a> StorePlace<'a> {
    pub fn try_store<V: TryStoreEffect>(
        &mut self,
        item: impl Into<V::MoveCtor>,
    ) -> Result<V, V::Error> {
        V::try_store(item.into(), self)
    }
}

pub trait StoreEffect {
    type MoveCtor;
    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut StorePlace<'a>) -> Self
    where
        Self: Sized;
}

pub trait TryStoreEffect {
    type MoveCtor;
    type Error;

    fn try_store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut StorePlace<'a>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized;
}

impl<T: CopyStorable> StoreEffect for T {
    type MoveCtor = T;

    #[inline]
    fn store<'a>(ctor: Self::MoveCtor, _in_place: &mut StorePlace<'a>) -> Self
    where
        Self: Sized,
    {
        ctor
    }
}

impl<T: CopyStorable> TryStoreEffect for T {
    type MoveCtor = T;
    type Error = Infallible;

    #[inline]
    fn try_store<'a>(
        ctor: Self::MoveCtor,
        _in_place: &mut StorePlace<'a>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized,
    {
        Ok(ctor)
    }
}

impl BaseType for () {}

unsafe impl<T: Invariant> Invariant for MaybeUninit<T> {}

mod test {

    use twizzler_derive::{BaseType, Invariant, NewStorer};

    use super::*;
    use crate::object::{Object, ObjectBuilder};
    #[derive(Invariant)]
    struct TestSE {
        pse: PhantomStoreEffect,
    }

    impl TestSE {
        fn new(sp: &mut StorePlace) -> Storer<Self> {
            unsafe {
                Storer::new_move(TestSE {
                    pse: PhantomStoreEffect,
                })
            }
        }
    }

    #[derive(Invariant, BaseType, NewStorer)]
    struct Foo {
        x: u32,
        se: TestSE,
    }

    #[derive(Invariant, BaseType, NewStorer)]
    struct Baz {
        x: u32,
        se: Option<TestSE>,
    }

    #[derive(Invariant, BaseType)]
    struct Bar {
        x: u32,
    }

    impl Bar {
        fn new() -> Self {
            Self { x: 32 }
        }
    }

    #[test]
    fn test_storer() {
        let obj_bar = ObjectBuilder::default().init(Bar::new()).unwrap();
        let obj: Object<Foo> = ObjectBuilder::default()
            .construct(|ci| Foo::new_storer(42, TestSE::new(&mut ci.in_place())))
            .unwrap();
        let obj_bar_ctor: Object<Bar> =
            ObjectBuilder::default().construct(|ci| Bar::new()).unwrap();

        let obj_baz: Object<Baz> = ObjectBuilder::default()
            .construct(|ci| Baz::new_storer(42, None))
            .unwrap();

        let obj_baz2: Object<Baz> = ObjectBuilder::default()
            .construct(|ci| Baz::new_storer(42, TestSE::new(&mut ci.in_place()).map(|x| Some(x))))
            .unwrap();
    }
}

impl<T: CopyStorable> From<T> for Storer<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

// TODO: impl all of these
impl<T> From<Option<Storer<T>>> for Storer<Option<T>> {
    fn from(value: Option<Storer<T>>) -> Self {
        match value {
            Some(s) => s.map(|s| Some(s)),
            None => unsafe { Storer::new_move(None) },
        }
    }
}