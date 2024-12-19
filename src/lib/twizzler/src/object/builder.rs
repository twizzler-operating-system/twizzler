use std::{alloc::AllocError, marker::PhantomData, mem::MaybeUninit};

use thiserror::Error;
use twizzler_abi::syscall::{ObjectCreate, ObjectCreateError};
use twizzler_rt_abi::object::{MapError, ObjectHandle};

use super::RawObject;
use crate::marker::BaseType;

#[derive(Clone, Copy, Debug, Error)]
/// Possible errors from creating an object.
pub enum CreateError {
    #[error(transparent)]
    Create(#[from] ObjectCreateError),
    #[error(transparent)]
    Map(#[from] MapError),
    #[error(transparent)]
    Alloc(#[from] AllocError),
}

/// An object builder, for constructing objects using a builder API.
pub struct ObjectBuilder<Base: BaseType> {
    spec: ObjectCreate,
    _pd: PhantomData<Base>,
}

impl<Base: BaseType> ObjectBuilder<Base> {
    /// Make a new object builder.
    pub fn new(spec: ObjectCreate) -> Self {
        Self {
            spec,
            _pd: PhantomData,
        }
    }
}

impl<Base: BaseType> Default for ObjectBuilder<Base> {
    fn default() -> Self {
        Self::new(ObjectCreate::default())
    }
}

/// An uninitialized object, used during object construction.
pub struct UninitObject<T> {
    handle: ObjectHandle,
    _pd: PhantomData<*mut MaybeUninit<T>>,
}

impl<T> RawObject for UninitObject<T> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}
