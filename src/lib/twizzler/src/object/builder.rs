use std::{alloc::AllocError, marker::PhantomData, mem::MaybeUninit};

use thiserror::Error;
use twizzler_abi::syscall::{LifetimeType, ObjectCreate, ObjectCreateError};
use twizzler_rt_abi::object::{MapError, MapFlags};

use super::Object;
use crate::{
    marker::{BaseType, StoreCopy},
    tx::TxObject,
};

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
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

    /// Make the object persistent.
    pub fn persist(mut self) -> Self {
        self.spec.lt = LifetimeType::Persistent;
        self
    }
}

impl<Base: BaseType + StoreCopy> ObjectBuilder<Base> {
    pub fn build(&self, base: Base) -> crate::tx::Result<Object<Base>> {
        self.build_inplace(|tx| tx.write(base))
    }
}

impl<Base: BaseType> ObjectBuilder<Base> {
    pub fn build_inplace<F>(&self, ctor: F) -> crate::tx::Result<Object<Base>>
    where
        F: FnOnce(TxObject<MaybeUninit<Base>>) -> crate::tx::Result<TxObject<Base>>,
    {
        let id = twizzler_abi::syscall::sys_object_create(self.spec, &[], &[])
            .map_err(CreateError::from)?;
        let mut flags = MapFlags::READ | MapFlags::WRITE;
        if self.spec.lt == LifetimeType::Persistent {
            flags.insert(MapFlags::PERSIST);
        }
        let mu_object = unsafe {
            Object::<MaybeUninit<Base>>::map_unchecked(id, flags).map_err(CreateError::from)
        }?;
        let object = ctor(mu_object.tx()?)?;
        object.commit()
    }
}

impl<Base: BaseType> Default for ObjectBuilder<Base> {
    fn default() -> Self {
        Self::new(ObjectCreate::default())
    }
}

#[cfg(test)]
mod tests {
    use super::ObjectBuilder;
    use crate::{marker::BaseType, object::TypedObject, ptr::InvPtr};

    #[test]
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

    #[test]
    fn builder_complex() {
        let builder = ObjectBuilder::default();
        let obj_1 = builder.build(42u32).unwrap();
        let base = obj_1.base();
        assert_eq!(*base, 42);

        let builder = ObjectBuilder::<Foo>::default();
        let obj = builder
            .build_inplace(|mut tx| {
                let foo = Foo {
                    ptr: InvPtr::new(&mut tx, base)?,
                };
                tx.write(foo)
            })
            .unwrap();
        let base_foo = obj.base();
        let r = unsafe { base_foo.ptr.resolve() };
        assert_eq!(*r, 42);
    }
}
