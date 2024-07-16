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

pub unsafe trait InPlaceCtor {
    type Builder;

    fn in_place_ctor<'b>(
        builder: Self::Builder,
        place: &'b mut MaybeUninit<Self>,
        tx: impl TxHandle<'b>,
    ) -> &'b mut Self
    where
        Self: Sized;
}

unsafe impl<T: Copy> InPlaceCtor for T {
    type Builder = T;
    fn in_place_ctor<'b>(
        builder: T,
        place: &'b mut MaybeUninit<Self>,
        _tx: impl TxHandle<'b>,
    ) -> &'b mut Self {
        place.write(builder)
    }
}
