use crate::{
    object::{BaseType, ImmutableObject, InitializedObject, MutableObject, Object},
    ptr::{InvPtr, InvSlice},
};

#[repr(C)]
pub struct VectorHeader<T> {
    base: InvSlice<T>,
    len: u64,
}

impl<T> BaseType for VectorHeader<T> {}

trait ArrayObject<T> {
    fn get(&self, idx: usize) -> Option<&T>;
    fn set(&self, idx: usize, val: T);
    fn push(&self, val: T);
    fn pop(&self) -> Option<T>;
}

impl<T> ArrayObject<T> for Object<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        todo!()
    }

    fn push(&self, val: T) {
        todo!()
    }

    fn pop(&self) -> Option<T> {
        todo!()
    }
}

impl<T> ArrayObject<T> for MutableObject<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        todo!()
    }

    fn push(&self, val: T) {
        todo!()
    }

    fn pop(&self) -> Option<T> {
        todo!()
    }
}

impl<T> ArrayObject<T> for ImmutableObject<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        panic!("cannot write to immutable object")
    }

    fn push(&self, val: T) {
        panic!("cannot write to immutable object")
    }

    fn pop(&self) -> Option<T> {
        panic!("cannot write to immutable object")
    }
}
