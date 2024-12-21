use std::mem::MaybeUninit;

use super::Allocator;
use crate::{
    marker::{Invariant, Storable},
    ptr::InvPtr,
    tx::{Result, TxHandle},
};

pub struct InvBox<T: Invariant, Alloc: Allocator> {
    raw: InvPtr<T>,
    alloc: Alloc,
}

impl<T: Invariant, Alloc: Allocator> InvBox<T, Alloc> {
    pub unsafe fn from_invptr(raw: InvPtr<T>, alloc: Alloc) -> Self {
        todo!()
    }

    pub fn new_in(val: T, alloc: Alloc, tx: &impl TxHandle) -> Result<Storable<Self>> {
        todo!()
    }

    pub fn new_inplace(place: &mut MaybeUninit<Self>, item: T, alloc: Alloc) -> Result<()> {
        todo!()
    }

    pub fn new_inplace_with<F>(place: &mut MaybeUninit<Self>, ctor: F, alloc: Alloc) -> Result<()>
    where
        F: FnOnce(&mut MaybeUninit<T>) -> Result<()>,
    {
        todo!()
    }
}

mod tests {
    use std::{mem::MaybeUninit, ptr::addr_of_mut};

    use super::InvBox;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaBase, ArenaObject},
        marker::{BaseType, Storable},
        object::{ObjectBuilder, TypedObject},
        tx::TxHandle,
    };

    struct Foo {
        x: InvBox<u32, ArenaAllocator>,
    }

    impl Foo {
        pub fn new_inplace<
            F: FnOnce(&mut MaybeUninit<InvBox<u32, ArenaAllocator>>) -> crate::tx::Result<()>,
        >(
            place: &mut MaybeUninit<Self>,
            ctor: F,
        ) -> crate::tx::Result<()> {
            let ptr_place = place.as_mut_ptr();

            Ok(())
        }
    }

    impl BaseType for Foo {}
    fn box_simple() {
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new().allocator();
        let obj = builder
            .build_inplace(|mut uo| {
                let place = uo.base_mut().as_mut_ptr();
                let ptr_place = unsafe { addr_of_mut!((*place).x) };
                let ptr = unsafe {
                    ptr_place
                        .cast::<MaybeUninit<InvBox<u32, ArenaAllocator>>>()
                        .as_mut()
                        .unwrap()
                };
                InvBox::new_inplace(ptr, 3, alloc)
            })
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.raw.resolve(), 3);
    }
}
