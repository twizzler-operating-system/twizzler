use std::mem::MaybeUninit;

use crate::tx::TxHandle;

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

pub struct InPlace<'a, T> {
    place: &'a mut MaybeUninit<T>,
}

impl<'a, T> InPlace<'a, T> {
    pub(crate) fn new(place: &'a mut MaybeUninit<T>) -> Self {
        Self { place }
    }

    pub fn place(&mut self) -> &mut MaybeUninit<T> {
        self.place
    }

    // This function is only safe because we never actually store through these.
    fn cast<V>(&mut self) -> InPlace<'a, V> {
        unsafe {
            InPlace {
                place: &mut *(self.place.as_mut_ptr() as *mut MaybeUninit<V>),
            }
        }
    }
}

impl<'a, T> InPlace<'a, T> {
    pub fn store<V: StoreEffect + 'a>(
        &mut self,
        item: impl Into<V::MoveCtor>,
        tx: impl TxHandle<'a>,
    ) -> V {
        V::store(item.into(), &mut self.cast(), tx)
    }
}

impl<'a, T> InPlace<'a, T> {
    pub fn try_store<V: TryStoreEffect + 'a>(
        &mut self,
        item: impl Into<V::MoveCtor>,
        tx: impl TxHandle<'a>,
    ) -> Result<V, V::Error> {
        V::try_store(item.into(), &mut self.cast(), tx)
    }
}

pub trait StoreEffect {
    type MoveCtor;
    fn store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut InPlace<'a, Self>,
        tx: impl TxHandle<'a>,
    ) -> Self
    where
        Self: Sized;
}

pub trait TryStoreEffect {
    type MoveCtor;
    type Error;

    fn try_store<'a>(
        ctor: Self::MoveCtor,
        in_place: &mut InPlace<'a, Self>,
        tx: impl TxHandle<'a>,
    ) -> Result<Self, Self::Error>
    where
        Self: Sized;
}
