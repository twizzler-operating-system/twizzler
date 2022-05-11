use std::marker::PhantomData;

use crate::ptr::InvPtr;

#[repr(transparent)]
pub struct InvRef<'a, T> {
    p: InvPtr<T>,
    _pd: PhantomData<&'a T>,
}

impl<'a, T> InvRef<'a, T> {
    pub fn lea<'b>(&'b self) -> EffectiveAddress<'b, T> {
        todo!()
    }
    //fn resolve(&self) ->
}

pub struct EffectiveAddress<'a, T> {
    raw: u64,
    fote: u32,
    _pd: PhantomData<&'a T>,
}
