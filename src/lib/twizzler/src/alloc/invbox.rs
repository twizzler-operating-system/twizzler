use std::mem::MaybeUninit;

use super::{Allocator, OwnedGlobalPtr};
use crate::{
    marker::Invariant,
    ptr::{GlobalPtr, InvPtr, Ref},
    tx::{Result, TxHandle, TxObject},
};

pub struct InvBox<T: Invariant, Alloc: Allocator> {
    raw: InvPtr<T>,
    alloc: Alloc,
}

impl<T: Invariant, Alloc: Allocator> InvBox<T, Alloc> {
    pub unsafe fn from_invptr(raw: InvPtr<T>, alloc: Alloc) -> Self {
        todo!()
    }

    pub fn new<B>(tx: &TxObject<B>, ogp: OwnedGlobalPtr<T, Alloc>) -> Self {
        todo!()
    }

    pub fn resolve(&self) -> Ref<'_, T> {
        todo!()
    }

    pub fn global(&self) -> GlobalPtr<T> {
        todo!()
    }

    pub fn as_ptr(&self) -> &InvPtr<T> {
        &self.raw
    }
}

mod tests {
    use std::{mem::MaybeUninit, ptr::addr_of_mut};

    use super::InvBox;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaBase, ArenaObject},
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
        tx::{TxHandle, TxObject},
    };

    struct Foo {
        x: InvBox<u32, ArenaAllocator>,
    }
    impl BaseType for Foo {}

    fn box_simple() {
        let alloc = ArenaObject::new();
        let foo = alloc
            .alloc_inplace(|tx| {
                let x = InvBox::new(tx.tx(), alloc.alloc(3));
                tx.write(Foo { x })
            })
            .unwrap();

        let base = foo.resolve();
        assert_eq!(*base.x.resolve(), 3);
    }

    fn box_simple_builder() {
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new();
        let obj = builder
            .build_inplace(|tx| {
                let x = InvBox::new(&tx, alloc.alloc(3));
                tx.write(Foo { x })
            })
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.resolve(), 3);
    }
}
