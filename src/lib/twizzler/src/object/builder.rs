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
    marker::{InPlace, Invariant},
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
    pub fn base<Base>(&self) -> InPlace<'_, Base> {
        // Safety: we are constructing an &mut to a MaybeUninit, which is safe. We guarantee that we
        // are the only reference, since the object is uninitialized and we have exclusive rights.
        let base = unsafe { &mut *(self.handle.base_mut_ptr() as *mut MaybeUninit<Base>) };
        InPlace::new(base)
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

    /// Get the uninitialized object that is being constructed.
    pub fn object(&self) -> &UninitializedObject {
        &self.object
    }

    pub fn base(&self) -> InPlace<'_, Base> {
        self.object().base()
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
    pub fn static_alloc_with<T: Invariant, StaticCtor>(
        &mut self,
        ctor: StaticCtor,
    ) -> Result<InvPtrBuilder<T>, AllocError>
    where
        StaticCtor: FnOnce(&mut Self, &mut InPlace<'_, T>) -> Result<T, AllocError>,
    {
        let (ptr, offset) = self.do_static_alloc::<MaybeUninit<T>>()?;
        unsafe {
            // Safety: we are taking an &mut to a MaybeUninit.
            let mut in_place = InPlace::new(&mut *ptr);
            let value = ctor(self, &mut in_place)?;
            in_place.place().write(value);
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
    pub fn construct<BaseCtor>(&self, ctor: BaseCtor) -> Result<Object<Base>, CreateError>
    where
        BaseCtor: FnOnce(&mut ConstructorInfo<Base>) -> Base,
    {
        let handle = self.create_object()?;
        let mut ci = ConstructorInfo::new(handle);
        unsafe {
            let base = ctor(&mut ci);
            ci.object.base().place().write(base);
            Ok(Object::new(ci.object.handle))
        }
    }

    /// Construct the object, building the Base value in-place.
    pub fn try_construct<BaseCtor>(&self, ctor: BaseCtor) -> Result<Object<Base>, CreateError>
    where
        BaseCtor: FnOnce(&mut ConstructorInfo<Base>) -> Result<Base, CreateError>,
    {
        let handle = self.create_object()?;
        let mut ci = ConstructorInfo::new(handle);
        unsafe {
            let base = ctor(&mut ci)?;
            ci.object().base().place().write(base);
            Ok(Object::new(ci.object.handle))
        }
    }
}

impl<Base: BaseType + Copy> ObjectBuilder<Base> {
    /// Construct the object, using the supplied base value.
    pub fn init(&self, base: Base) -> Result<Object<Base>, CreateError> {
        let handle = self.create_object()?;
        unsafe {
            (handle.handle.base_mut_ptr() as *mut Base).write(base);
            Ok(Object::new(handle.handle))
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        marker::InPlace,
        object::{builder::ObjectInitTxHandle, BaseType, InitializedObject, Object, ObjectBuilder},
        ptr::{InvPtr, InvPtrBuilder},
        tx::TxHandle,
    };

    #[derive(twizzler_derive::InvariantCopy, Clone, Copy)]
    #[repr(C)]
    struct Foo {
        x: u32,
    }
    impl BaseType for Foo {}

    #[derive(twizzler_derive::Invariant)]
    #[repr(C)]
    struct Bar {
        x: InvPtr<Foo>,
        y: InvPtr<Foo>,
        z: InvPtr<Bar>,
    }

    impl Bar {
        fn new<'a>(
            in_place: &mut InPlace<'a, Bar>,
            x: InvPtrBuilder<Foo>,
            y: InvPtrBuilder<Foo>,
            z: InvPtrBuilder<Bar>,
            tx: &impl TxHandle<'a>,
        ) -> Self {
            Self {
                x: in_place.store(x, tx),
                y: in_place.store(y, tx),
                z: in_place.store(z, tx),
            }
        }
    }
    impl BaseType for Bar {}

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
                Bar {
                    x: ci.base().store(foo_alloc, ObjectInitTxHandle),
                    y: InvPtr::null(),
                    z: InvPtr::null(),
                }
            })
            .unwrap();
        assert_eq!(bar_obj.base().x.resolve().unwrap().x, 46);
    }

    #[test]
    fn construct_object_with_static_alloc() {
        let builder = ObjectBuilder::default();
        let foo_obj = builder.init(Foo { x: 12345 }).unwrap();

        let builder = ObjectBuilder::default();
        let bar_obj: Object<Bar> = builder
            .try_construct(|ci| {
                let static_foo_alloc_a1 = ci.static_alloc(Foo { x: 1 })?;

                println!("HERE");
                let static_bar_alloc = ci.static_alloc_with(|ci, in_place| {
                    let static_foo_alloc_b1 = ci.static_alloc(Foo { x: 101 })?;
                    let static_foo_alloc_b2 = ci.static_alloc(Foo { x: 102 })?;
                    Ok(Bar::new(
                        in_place,
                        static_foo_alloc_b1,
                        static_foo_alloc_b2,
                        InvPtrBuilder::null(),
                        &ObjectInitTxHandle,
                    ))
                })?;
                Ok(Bar::new(
                    &mut ci.base(),
                    foo_obj.base().into(),
                    static_foo_alloc_a1,
                    static_bar_alloc,
                    &ObjectInitTxHandle,
                ))
            })
            .unwrap();

        let foo_obj_x = bar_obj.base().x.resolve().unwrap().x;
        let yx = bar_obj.base().y.resolve().unwrap().x;
        let zxx = bar_obj.base().z.resolve().unwrap().x.resolve().unwrap().x;
        let zyx = bar_obj.base().z.resolve().unwrap().y.resolve().unwrap().x;
        assert_eq!(foo_obj_x, 12345);
        assert_eq!(yx, 1);
        assert_eq!(zxx, 101);
        assert_eq!(zyx, 102);
    }
}
