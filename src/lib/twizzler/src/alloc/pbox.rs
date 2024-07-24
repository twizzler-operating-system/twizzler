use std::{
    alloc::{AllocError, Layout},
    marker::PhantomData,
    ops::Deref,
};

use super::Allocator;
use crate::{
    marker::{InPlace, StoreEffect},
    object::InitializedObject,
    ptr::{GlobalPtr, InvPtr, InvPtrBuilder},
    tx::{TxHandle, TxResult},
};

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct PBox<T, A: Allocator> {
    ptr: InvPtr<T>,
    alloc: A,
}

pub struct PBoxBuilder<T, A: Allocator> {
    inv: InvPtrBuilder<T>,
    alloc: A,
}

impl<T, A: Allocator> Deref for PBox<T, A> {
    type Target = InvPtr<T>;

    fn deref(&self) -> &Self::Target {
        &self.ptr
    }
}

impl<T, A: Allocator> PBox<T, A> {
    pub unsafe fn from_invptr(ptr: InvPtr<T>, alloc: A) -> Self {
        Self { ptr, alloc }
    }

    pub fn new_in(value: T, alloc: A) -> Result<PBoxBuilder<T, A>, AllocError> {
        let gptr = alloc.allocate(Layout::new::<T>())?.cast::<T>();
        let ptr = gptr.resolve().map_err(|_| AllocError)?;
        let mut mut_ptr = unsafe { ptr.as_mut() };
        *mut_ptr = value;

        Ok(PBoxBuilder {
            inv: unsafe { InvPtrBuilder::from_global(gptr) },
            alloc,
        })
    }

    pub fn new_in_with(
        ctor: impl FnOnce(InPlace) -> T,
        alloc: A,
    ) -> Result<PBoxBuilder<T, A>, AllocError> {
        let gptr = alloc.allocate(Layout::new::<T>())?.cast::<T>();
        let ptr = gptr.resolve().map_err(|_| AllocError)?;
        let mut mut_ptr = unsafe { ptr.as_mut() };
        let in_place = InPlace::new(&ptr.handle());
        *mut_ptr = ctor(in_place);

        Ok(PBoxBuilder {
            inv: unsafe { InvPtrBuilder::from_global(gptr) },
            alloc,
        })
    }
}

impl<T, A: Allocator> Drop for PBox<T, A> {
    fn drop(&mut self) {
        if let Ok(res) = self.ptr.resolve() {
            unsafe {
                let ptr = res.ptr() as *mut T;
                core::ptr::drop_in_place(ptr);
            }
        } else {
            //TODO
        }

        if let Ok(res) = self.ptr.as_global() {
            // TODO
            let _ = unsafe { self.alloc.deallocate(res.cast(), Layout::new::<T>()) };
        } else {
            //TODO
        }
    }
}

impl<T, A: Allocator> StoreEffect for PBox<T, A> {
    type MoveCtor = PBoxBuilder<T, A>;

    fn store<'a>(ctor: Self::MoveCtor, in_place: &mut InPlace<'a>) -> Self
    where
        Self: Sized,
    {
        unsafe { PBox::from_invptr(in_place.store(ctor.inv), ctor.alloc) }
    }
}

//#[cfg(test)]
mod test {
    use std::u32;

    use twizzler_abi::syscall::ObjectCreate;

    use super::{PBox, PBoxBuilder};
    use crate::{
        alloc::{
            arena::{ArenaAllocator, ArenaManifest},
            TxAllocator,
        },
        object::{BaseType, ConstructorInfo, InitializedObject, Object, ObjectBuilder},
        ptr::InvPtrBuilder,
        tx::{TxCell, TxHandle},
    };

    #[derive(twizzler_derive::Invariant)]
    #[repr(C)]
    struct Foo {
        data: PBox<Bar, ArenaAllocator>,
        data2: TxCell<u32>,
    }

    #[derive(twizzler_derive::Invariant, Copy, Clone)]
    #[repr(C)]
    struct Bar {
        x: u32,
    }

    #[test]
    fn test() {
        let obj = ObjectBuilder::default()
            .init(ArenaManifest::default())
            .unwrap();
        let arena = obj.base();

        let foo = arena
            .alloc_with(|mut ip| {
                let arena = ArenaAllocator::new(&mut ip, obj.base());
                Foo {
                    data: ip.store(PBox::new_in(Bar { x: 42 }, arena).unwrap()),
                    data2: TxCell::new(3),
                }
            })
            .unwrap();
    }
}
