use std::{alloc::AllocError, marker::PhantomData, mem::MaybeUninit};

use thiserror::Error;
use twizzler_abi::syscall::{ObjectCreate, ObjectCreateError};
use twizzler_rt_abi::object::{MapError, ObjectHandle};

use super::{Object, RawObject};
use crate::{
    marker::{BaseType, Storable, StoreCopy},
    tx::TxHandle,
};

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
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
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

impl<Base: BaseType + StoreCopy> ObjectBuilder<Base> {
    pub fn build(&self, base: Base) -> Result<Object<Base>, CreateError> {
        todo!()
    }
}

impl<Base: BaseType> ObjectBuilder<Base> {
    pub fn build_with<F>(self, ctor: F) -> Result<Object<Base>, CreateError>
    where
        F: FnOnce(UninitObject<Base>) -> Storable<Base>,
    {
        todo!()
    }

    pub fn build_inplace<F>(self, ctor: F) -> Result<Object<Base>, CreateError>
    where
        F: FnOnce(UninitObject<Base>) -> crate::tx::Result<()>,
    {
        todo!()
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

impl<T> UninitObject<T> {
    pub fn base_mut(&mut self) -> &mut MaybeUninit<T> {
        todo!()
    }

    pub fn base(&self) -> &MaybeUninit<T> {
        todo!()
    }
}

impl<T> RawObject for UninitObject<T> {
    fn handle(&self) -> &ObjectHandle {
        &self.handle
    }
}

impl<B> TxHandle for UninitObject<B> {
    fn tx_mut(&self, data: *const u8, len: usize) -> crate::tx::Result<*mut u8> {
        todo!()
    }
}
mod tests {
    use super::ObjectBuilder;
    use crate::{
        marker::{BaseType, Storable, StoreCopy},
        object::TypedObject,
        ptr::{InvPtr, Ref},
        tx::TxHandle,
    };

    fn builder_simple() {
        let builder = ObjectBuilder::default();
        let obj = builder.build(42u32).unwrap();
        let base = obj.base();
        assert_eq!(*base, 42);
    }

    struct Foo {
        ptr: InvPtr<u32>,
    }

    impl BaseType for Foo {}

    impl Foo {
        pub fn new_in(target: &impl TxHandle, ptr: Storable<InvPtr<u32>>) -> Storable<Self> {
            unsafe {
                Storable::new(Foo {
                    ptr: ptr.into_inner_unchecked(),
                })
            }
        }
    }

    fn builder_complex() {
        let builder = ObjectBuilder::default();
        let obj_1 = builder.build_with(|_uo| 42u32.into()).unwrap();
        let base = obj_1.base();
        assert_eq!(*base, 42);

        let builder = ObjectBuilder::<Foo>::default();
        let obj = builder
            .build_with(|uo| Foo::new_in(&uo, InvPtr::new_in(&uo, base)))
            .unwrap();
        let base_foo = obj.base();
        let r = base_foo.ptr.resolve();
        assert_eq!(*r, 42);
    }
}
