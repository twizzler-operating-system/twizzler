use std::mem::MaybeUninit;

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

unsafe impl<T: Invariant, const N: usize> Invariant for [T; N] {}

pub struct InPlace<'a> {
    place: &'a mut MaybeUninit<u8>,
}

impl<'a> InPlace<'a> {
    pub(crate) fn new(place: &'a mut MaybeUninit<u8>) -> Self {
        Self { place }
    }

    pub(crate) fn place(&mut self) -> &mut MaybeUninit<u8> {
        self.place
    }
}

impl<'a> InPlace<'a> {
    pub fn store<V: StoreEffect + 'a>(&mut self, item: impl Into<V::MoveCtor>) -> V {
        V::store(item.into(), self)
    }
}

impl<'a> InPlace<'a> {
    pub fn try_store<V: TryStoreEffect + 'a>(
        &mut self,
        item: impl Into<V::MoveCtor>,
    ) -> Result<V, V::Error> {
        V::try_store(item.into(), self)
    }
}

pub trait StoreEffect {
    type MoveCtor;
    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut InPlace<'a>) -> Self
    where
        Self: Sized;
}

pub trait TryStoreEffect {
    type MoveCtor;
    type Error;

    fn try_store<'a>(ctor: Self::MoveCtor, in_place: &mut InPlace<'a>) -> Result<Self, Self::Error>
    where
        Self: Sized;
}
