use std::marker::PhantomData;

use twizzler_abi::syscall::{ObjectCreate, ObjectCreateError};
use twizzler_runtime_api::ObjectHandle;

use super::{BaseType, Object};
use crate::{
    marker::InPlaceCtor,
    ptr::{InvPtr, InvPtrBuilder},
};

pub struct ObjectBuilder<Base: BaseType> {
    spec: ObjectCreate,
    _pd: PhantomData<Base>,
}

impl<Base: BaseType> Default for ObjectBuilder<Base> {
    fn default() -> Self {
        Self::new(ObjectCreate::default())
    }
}

pub struct UninitializedObject {
    handle: ObjectHandle,
}

pub struct ConstructorInfo<'a> {
    pub object: UninitializedObject,
    pub static_allocations: &'a [StaticAllocation],
}

impl<'a> ConstructorInfo<'a> {
    pub fn new_invptr<T>(&mut self, builder: InvPtrBuilder<T>) -> InvPtr<T> {
        todo!()
    }
}

pub struct StaticAllocation {
    offset: u64,
}

impl StaticAllocation {
    pub fn as_local_invptr<T>(&self) -> InvPtr<T> {
        todo!()
    }
}

impl<Base: BaseType> ObjectBuilder<Base> {
    pub fn new(spec: ObjectCreate) -> Self {
        Self {
            spec,
            _pd: PhantomData,
        }
    }

    pub fn allocate_static<T>(self, init: T) -> Self {
        todo!()
    }

    pub fn allocate_static_ctor<'a, T, StaticCtor>(self, ctor: StaticCtor) -> Self
    where
        StaticCtor: FnOnce(ConstructorInfo<'a>) -> T,
    {
        todo!()
    }
}

impl<Base: BaseType + InPlaceCtor> ObjectBuilder<Base> {
    pub fn construct<BaseCtor>(&self, ctor: BaseCtor) -> Result<Object<Base>, ObjectCreateError>
    where
        BaseCtor: FnOnce(ConstructorInfo<'_>) -> Base::Builder,
    {
        todo!()
    }
}

impl<Base: BaseType + Copy> ObjectBuilder<Base> {
    pub fn build(&self, base: Base) -> Result<Object<Base>, ObjectCreateError> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    use twizzler_abi::syscall::{BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags};

    use super::ObjectBuilder;
    use crate::{object::BaseType, ptr::InvPtr};
    const DEF_SPEC: ObjectCreate = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::empty(),
    );

    #[repr(C)]
    struct Foo {
        x: u32,
    }
    impl BaseType for Foo {}

    #[repr(C)]
    struct Bar {
        x: InvPtr<Foo>,
    }
    impl BaseType for Bar {}

    #[repr(C)]
    struct Baz {
        x: bool,
    }

    #[test]
    fn test() {
        let builder = ObjectBuilder::new(DEF_SPEC);
        let foo_obj = builder.construct(|_obj| Foo { x: 42 }).unwrap();

        let _another_foo_obj = ObjectBuilder::new(DEF_SPEC).build(Foo { x: 42 }).unwrap();

        let builder = ObjectBuilder::new(DEF_SPEC);
        let bar_obj = builder
            .construct(|mut ctorinfo| Bar {
                x: ctorinfo.new_invptr(foo_obj.base().into()),
            })
            .unwrap();
    }

    #[test]
    fn test_static() {
        let builder = ObjectBuilder::new(DEF_SPEC).allocate_static(Baz { x: true });
        let foo_obj = builder.construct(|_obj| Foo { x: 42 }).unwrap();

        let _another_foo_obj = ObjectBuilder::new(DEF_SPEC).build(Foo { x: 42 }).unwrap();

        let builder = ObjectBuilder::new(DEF_SPEC);
        let bar_obj = builder
            .construct(|mut ctorinfo| Bar {
                x: ctorinfo.new_invptr(foo_obj.base().into()),
            })
            .unwrap();
    }
}
