use std::{
    alloc::{AllocError, Layout},
    marker::PhantomData,
    mem::MaybeUninit,
};

use thiserror::Error;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_object_create, ObjectCreate, ObjectCreateError},
};
use twizzler_runtime_api::{get_runtime, MapError, MapFlags, ObjectHandle};

use super::{BaseType, Object};
use crate::{
    marker::{CopyStorable, Invariant, StorePlace, Storer},
    object::RawObject,
    ptr::InvPtrBuilder,
    tx::TxHandle,
};

#[derive(Clone, Copy, Debug, Error)]
pub enum CreateError {
    #[error(transparent)]
    Create(#[from] ObjectCreateError),
    #[error(transparent)]
    Map(#[from] MapError),
    #[error(transparent)]
    Alloc(#[from] AllocError),
}

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

impl UninitializedObject {
    pub(crate) fn in_place(&self) -> StorePlace<'_> {
        StorePlace::new(&self.handle)
    }
}

pub struct ConstructorInfo<Base> {
    object: UninitializedObject,
    static_alloc_offset: usize,
    _pd: PhantomData<MaybeUninit<Base>>,
}

impl<Base> ConstructorInfo<Base> {
    fn new(object: UninitializedObject) -> Self {
        Self {
            object,
            static_alloc_offset: Layout::new::<Base>().size() + NULLPAGE_SIZE,
            _pd: PhantomData,
        }
    }

    pub fn write_base<InitBase>(&self, base_init: InitBase)
    where
        InitBase: Into<Storer<Base>>,
        Base: Invariant,
    {
        let base = unsafe { &mut *(self.object.handle.base_mut_ptr() as *mut MaybeUninit<Base>) };
        base.write(base_init.into().into_inner());
    }

    /// Get the uninitialized object that is being constructed.
    pub fn object(&self) -> &UninitializedObject {
        &self.object
    }

    pub fn in_place(&self) -> StorePlace<'_> {
        self.object().in_place()
    }

    fn do_static_alloc<T>(&mut self) -> Result<(*mut T, usize), AllocError> {
        const MIN_ALIGN: usize = 32;
        let layout = Layout::new::<T>();
        let align = std::cmp::max(layout.align(), MIN_ALIGN);
        let offset = self.static_alloc_offset.next_multiple_of(align);
        let ptr = self
            .object
            .handle
            .lea_mut(offset, layout.size())
            .ok_or(AllocError)? as *mut T;
        self.static_alloc_offset = offset + layout.size();
        Ok((ptr, offset))
    }

    /// Allocate a value of type T in object memory at creation time.
    pub fn static_alloc<T: Invariant>(&mut self, value: T) -> Result<InvPtrBuilder<T>, AllocError> {
        let (ptr, offset) = self.do_static_alloc::<T>()?;
        unsafe {
            // Safety: the object is still uninitialized, so no references exist. We can blindly
            // write to initialize the value at the pointed-to memory.
            ptr.write(value);
            // Safety: we just initialized this value above.
            Ok(InvPtrBuilder::from_offset(offset))
        }
    }

    /// Allocate a value of type T in object memory at creation time, and construct it in-place.
    pub fn static_alloc_with<T: Invariant, StaticCtor, ST>(
        &mut self,
        ctor: StaticCtor,
    ) -> Result<InvPtrBuilder<T>, AllocError>
    where
        StaticCtor: FnOnce(&mut Self) -> Result<ST, AllocError>,
        ST: Into<Storer<T>>,
    {
        let (ptr, offset) = self.do_static_alloc::<MaybeUninit<T>>()?;
        unsafe {
            // Safety: we are taking an &mut to a MaybeUninit.
            let value = ctor(self)?;
            (&mut *ptr).write(value.into().into_inner());
            // Safety: we just initialized this value above.
            Ok(InvPtrBuilder::from_offset(offset))
        }
    }
}

impl<Base: BaseType> ObjectBuilder<Base> {
    /// Create a new object builder.
    pub fn new(spec: ObjectCreate) -> Self {
        Self {
            spec,
            _pd: PhantomData,
        }
    }

    /// Create the object without initializing it.
    pub fn create_object(&self) -> Result<UninitializedObject, CreateError> {
        let id = sys_object_create(self.spec, &[], &[])?;
        let handle = get_runtime().map_object(id, MapFlags::READ | MapFlags::WRITE)?;
        println!("MAPPED: {:p}", handle.start);
        Ok(UninitializedObject { handle })
    }
}

struct ObjectInitTxHandle;

impl<'a> TxHandle<'a> for ObjectInitTxHandle {
    fn tx_mut<T, E>(&self, data: *const T) -> crate::tx::TxResult<*mut T, E> {
        Ok(data as *mut T)
    }
}

impl<Base: BaseType + Invariant> ObjectBuilder<Base> {
    /// Construct the object, building the Base value in-place.
    pub fn construct<BaseCtor, IntoBase>(&self, ctor: BaseCtor) -> Result<Object<Base>, CreateError>
    where
        BaseCtor: FnOnce(&mut ConstructorInfo<Base>) -> IntoBase,
        IntoBase: Into<Storer<Base>>,
    {
        let handle = self.create_object()?;
        let mut ci = ConstructorInfo::new(handle);
        unsafe {
            let base = ctor(&mut ci);
            ci.write_base(base);
            Ok(Object::new(ci.object.handle))
        }
    }

    /// Construct the object, building the Base value in-place.
    pub fn try_construct<BaseCtor, IntoBase>(
        &self,
        ctor: BaseCtor,
    ) -> Result<Object<Base>, CreateError>
    where
        BaseCtor: FnOnce(&mut ConstructorInfo<Base>) -> Result<IntoBase, CreateError>,
        IntoBase: Into<Storer<Base>>,
    {
        let handle = self.create_object()?;
        let mut ci = ConstructorInfo::new(handle);
        unsafe {
            let base = ctor(&mut ci)?;
            ci.write_base(base);
            Ok(Object::new(ci.object.handle))
        }
    }
}

impl<Base: BaseType + CopyStorable + Invariant> ObjectBuilder<Base> {
    /// Construct the object, using the supplied base value.
    pub fn init(&self, base: Base) -> Result<Object<Base>, CreateError> {
        let handle = self.create_object()?;
        unsafe {
            (handle.handle.base_mut_ptr() as *mut Base).write(base.into());
            Ok(Object::new(handle.handle))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        marker::{StorePlace, Storer},
        object::{builder::ObjectInitTxHandle, BaseType, InitializedObject, Object, ObjectBuilder},
        ptr::{InvPtr, InvPtrBuilder},
        tx::TxHandle,
    };

    #[derive(twizzler_derive::Invariant, Clone, Copy, twizzler_derive::BaseType)]
    #[repr(C)]
    struct Foo {
        x: u32,
    }

    #[derive(twizzler_derive::Invariant, twizzler_derive::NewStorer, twizzler_derive::BaseType)]
    #[repr(C)]
    struct Bar {
        x: InvPtr<Foo>,
        y: InvPtr<Foo>,
        z: InvPtr<Bar>,
    }

    #[test]
    fn init_object() {
        let foo_obj = ObjectBuilder::default().init(Foo { x: 42 }).unwrap();
        assert_eq!(foo_obj.base().x, 42);
    }

    #[test]
    fn simple_object_with_static_alloc() {
        let bar_obj: Object<Bar> = ObjectBuilder::default()
            .construct(|ci| {
                let foo_alloc = ci.static_alloc(Foo { x: 46 }).unwrap();
                Bar::new_storer(
                    Storer::store(foo_alloc, &mut ci.in_place()),
                    Storer::store(InvPtrBuilder::null(), &mut ci.in_place()),
                    Storer::store(InvPtrBuilder::null(), &mut ci.in_place()),
                )
            })
            .unwrap();
        assert!(bar_obj.base().x.is_local());
        assert_eq!(unsafe { bar_obj.base().x.resolve().x }, 46);
    }

    #[test]
    fn construct_object_with_static_alloc() {
        let builder = ObjectBuilder::default();
        let foo_obj = builder.init(Foo { x: 12345 }).unwrap();

        let builder = ObjectBuilder::default();
        let bar_obj: Object<Bar> = builder
            .try_construct(|ci| {
                let static_foo_alloc_a1 = ci.static_alloc(Foo { x: 1 })?;

                let static_bar_alloc = ci.static_alloc_with(|ci| {
                    let static_foo_alloc_b1 = ci.static_alloc(Foo { x: 101 })?;
                    let static_foo_alloc_b2 = ci.static_alloc(Foo { x: 102 })?;

                    Ok(Bar::new_storer(
                        Storer::store(static_foo_alloc_b1, &mut ci.in_place()),
                        Storer::store(static_foo_alloc_b2, &mut ci.in_place()),
                        Storer::store(InvPtrBuilder::null(), &mut ci.in_place()),
                    ))
                })?;

                Ok(Bar::new_storer(
                    Storer::store(foo_obj.base(), &mut ci.in_place()),
                    Storer::store(static_foo_alloc_a1, &mut ci.in_place()),
                    Storer::store(static_bar_alloc, &mut ci.in_place()),
                ))
            })
            .unwrap();

        let foo_obj_x = unsafe { bar_obj.base().x.resolve().x };
        assert!(!bar_obj.base().x.is_local());
        let yx = unsafe { bar_obj.base().y.resolve().x };
        let zxx = unsafe { bar_obj.base().z.resolve().x.resolve().x };
        let zyx = unsafe { bar_obj.base().z.resolve().y.resolve().x };
        assert_eq!(foo_obj_x, 12345);
        assert_eq!(yx, 1);
        assert_eq!(zxx, 101);
        assert_eq!(zyx, 102);
    }
}
