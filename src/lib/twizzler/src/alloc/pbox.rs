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

    pub fn new_in(value: T, alloc: A) -> Result<PBoxBuilder<T, A>, AllocError>
    where
        T: Unpin,
    {
        let gptr = alloc.allocate(Layout::new::<T>())?.cast::<T>();
        let ptr = unsafe { gptr.resolve().map_err(|_| AllocError) }?;
        let mut mut_ptr = unsafe { ptr.into_mut() };
        *mut_ptr = value;

        Ok(PBoxBuilder {
            inv: InvPtrBuilder::from_global(gptr),
            alloc,
        })
    }

    pub fn new_in_with(
        ctor: impl FnOnce(InPlace) -> T,
        alloc: A,
    ) -> Result<PBoxBuilder<T, A>, AllocError> {
        let gptr = alloc.allocate(Layout::new::<T>())?.cast::<T>();
        let ptr = unsafe { gptr.resolve().map_err(|_| AllocError) }?;
        let mut_ptr = unsafe { ptr.into_mut() };
        let in_place = InPlace::new(&mut_ptr.handle());
        unsafe { mut_ptr.ptr().write(ctor(in_place)) };

        Ok(PBoxBuilder {
            inv: InvPtrBuilder::from_global(gptr),
            alloc,
        })
    }
}

impl<T, A: Allocator> Drop for PBox<T, A> {
    fn drop(&mut self) {
        if let Ok(res) = unsafe { self.ptr.try_resolve() } {
            unsafe {
                let ptr = res.ptr() as *mut T;
                core::ptr::drop_in_place(ptr);
            }
        } else { //TODO
        }

        if let Ok(res) = self.ptr.try_as_global() {
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
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        u32,
    };

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
    struct Node {
        next: Option<PBox<Node, ArenaAllocator>>,
        value: u32,
    }

    #[derive(twizzler_derive::Invariant)]
    #[repr(C)]
    struct Root {
        list: PBox<Node, ArenaAllocator>,
    }

    impl BaseType for Root {}

    #[test]
    fn test() {
        let obj = ObjectBuilder::default()
            .init(ArenaManifest::default())
            .unwrap();
        let arena = obj.base();

        let alloc_node = |parent: Option<PBoxBuilder<Node, ArenaAllocator>>,
                          value: u32,
                          arena: &ArenaManifest| {
            PBox::new_in_with(
                |mut ip| Node {
                    next: parent.map(|parent| ip.store(parent)),
                    value,
                },
                ArenaAllocator::new(&*arena),
            )
            .unwrap()
        };

        let node1 = alloc_node(None, 3, &arena);
        let node2 = alloc_node(Some(node1), 11, &arena);

        let root_object = ObjectBuilder::default()
            .construct(|ci| Root {
                list: ci.in_place().store(node2),
            })
            .unwrap();

        let root = root_object.base();
        let res_node2 = unsafe { root.list.resolve() };
        let value2 = res_node2.value;
        let res_node1 = unsafe { res_node2.next.as_ref().unwrap().resolve() };
        let value1 = res_node1.value;
        assert!(res_node1.next.is_none());
        assert_eq!(value1, 3);
        assert_eq!(value2, 11);
    }
}
