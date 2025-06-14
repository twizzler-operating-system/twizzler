use std::{marker::PhantomData, mem::MaybeUninit};

use twizzler_abi::{
    object::Protections,
    syscall::{
        BackingType, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_rt_abi::object::MapFlags;

use super::Object;
use crate::{
    marker::{BaseType, StoreCopy},
    tx::TxObject,
};

/// An object builder, for constructing objects using a builder API.
#[derive(Clone)]
pub struct ObjectBuilder<Base: BaseType> {
    spec: ObjectCreate,
    src_objs: Vec<ObjectSource>,
    ties: Vec<CreateTieSpec>,
    _pd: PhantomData<Base>,
}

impl<Base: BaseType> ObjectBuilder<Base> {
    /// Make a new object builder.
    pub fn new(spec: ObjectCreate) -> Self {
        Self {
            spec,
            _pd: PhantomData,
            src_objs: Vec::new(),
            ties: Vec::new(),
        }
    }

    /// Make the object persistent.
    pub fn persist(mut self) -> Self {
        self.spec.lt = LifetimeType::Persistent;
        self
    }

    /// Cast the base type.
    pub fn cast<U: BaseType>(self) -> ObjectBuilder<U> {
        ObjectBuilder::<U>::new(self.spec)
    }

    /// Add a Source Object that this new object will copy from.
    pub fn add_src(mut self, obj_src: ObjectSource) -> Self {
        self.src_objs.push(obj_src);
        self
    }

    /// Add a tie specification for this object creation.
    pub fn add_tie(mut self, tie: CreateTieSpec) -> Self {
        self.ties.push(tie);
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
        let id = twizzler_abi::syscall::sys_object_create(
            self.spec,
            self.src_objs.as_slice(),
            self.ties.as_slice(),
        )?;
        let mut flags = MapFlags::READ | MapFlags::WRITE;
        if self.spec.lt == LifetimeType::Persistent {
            flags.insert(MapFlags::PERSIST);
        }
        let mu_object = unsafe { Object::<MaybeUninit<Base>>::map_unchecked(id, flags) }?;
        let object = ctor(mu_object.tx()?)?;
        object.commit()
    }

    pub fn build_ctor<F>(&self, ctor: F) -> crate::tx::Result<Object<Base>>
    where
        F: FnOnce(&mut TxObject<MaybeUninit<Base>>),
    {
        let id = twizzler_abi::syscall::sys_object_create(
            self.spec,
            self.src_objs.as_slice(),
            self.ties.as_slice(),
        )?;
        let mut flags = MapFlags::READ | MapFlags::WRITE;
        if self.spec.lt == LifetimeType::Persistent {
            flags.insert(MapFlags::PERSIST);
        }
        let mu_object = unsafe { Object::<MaybeUninit<Base>>::map_unchecked(id, flags) }?;
        let mut tx = mu_object.tx()?;
        ctor(&mut tx);
        Ok(unsafe { tx.commit()?.cast() })
    }
}

impl<Base: BaseType> Default for ObjectBuilder<Base> {
    fn default() -> Self {
        Self::new(ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
            Protections::all(),
        ))
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
        let base = obj_1.base_ref();
        assert_eq!(*base, 42);

        let builder = ObjectBuilder::<Foo>::default();
        let obj = builder
            .build_inplace(|tx| {
                let foo = Foo {
                    ptr: InvPtr::new(&tx, base)?,
                };
                tx.write(foo)
            })
            .unwrap();
        let base_foo = obj.base();
        let r = unsafe { base_foo.ptr.resolve() };
        assert_eq!(*r, 42);
    }
}
