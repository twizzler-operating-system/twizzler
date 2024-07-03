//! Object construction APIs.
//!
//! The primary mechanism for creating objects in Twizzler uses the Builder pattern. An
//! [ObjectBuilder] allows a programmer a number of convenience features for creating objects,
//! statically allocating regions within them at construction time, and safely initializing the Base
//! of the object.
//!
//! # Simple Base types (can be constructed out-of-place)
//! For simple base types that can be constructed anywhere and copied into the object, the object
//! builder provides a [ObjectBuilder::build] method that initialized the object base and constructs
//! the object, returning an open handle to the new object.
//!
//! ```{rust}
//! struct Foo { x: u32 }
//! impl BaseType for Foo {}
//! const DEF_SPEC: ObjectCreate = ObjectCreate::new(BackingType::Normal, LifetimeType::Volatile, None, ObjectCreateFlags::empty());
//! let foo_object = ObjectBuilder::new(DEF_SPEC).build(Foo {x: 42}).unwrap();
//! ```
//!
//! # Complex Base types
//! Some base types require arbitrary work to build, and for these, we provide a
//! [ObjectBuilder::construct] method that takes a closure that constructs the object base type.
//! When this closure runs, it has access to a [ConstructorInfo] struct that contains a reference to
//! the object being created as an _uninitialized object_ (since by definition we have not yet
//! constructed the base).
//!
//! ```{rust}
//! struct Foo { x: u32 }
//! impl BaseType for Foo {}
//! const DEF_SPEC: ObjectCreate = ObjectCreate::new(BackingType::Normal, LifetimeType::Volatile, None, ObjectCreateFlags::empty());
//! let foo_object = ObjectBuilder::new(DEF_SPEC).construct(|info| Foo {x: 42}).unwrap();
//! ```
//!
//! # Static allocations
//! A common pattern is to allocate, statically, a region of the object to be used during runtime,
//! and then point to it from the base of the object. The [ObjectBuilder] provides the
//! `allocate_static` function for this, which takes a value of type T, and then allocates space for
//! the T in the object, after the base data. For example, the Queue object contains, in its base, a
//! pointer to the buffer, which is contained in the same object.
//!
//! Here is an example that constructs such an object:
//!
//! ```{rust}
//! let builder = ObjectBuilder::new(DEF_SPEC);
//! let foo_obj = builder.construct(|_obj| Foo { x: 42 }).unwrap();
//!
//! let builder = ObjectBuilder::new(DEF_SPEC);
//! let bar_obj = builder.construct(|mut ctorinfo| {
//!     // Build an invariant pointer to foo_obj's base data. This is a little funky here because it's bootstrapping -- the new object hasn't been constructed yet, so we can't safely use it directly yet.
//!     let foo_ptr = ctorinfo.new_invptr(foo_obj.base().into());
//!     Bar { x: foo_ptr }
//! }).unwrap();

use std::marker::PhantomData;

use twizzler_abi::syscall::{ObjectCreate, ObjectCreateError};
use twizzler_runtime_api::ObjectHandle;

use super::{BaseType, Object};
use crate::ptr::{InvPtr, InvPtrBuilder};

pub struct ObjectBuilder<Base: BaseType> {
    spec: ObjectCreate,
    _pd: PhantomData<Base>,
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

    pub fn construct<BaseCtor>(&self, ctor: BaseCtor) -> Result<Object<Base>, ObjectCreateError>
    where
        BaseCtor: FnOnce(ConstructorInfo<'_>) -> Base,
    {
        todo!()
    }

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
