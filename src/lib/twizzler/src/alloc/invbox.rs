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
}

mod tests {
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
        pub fn new_in(
            target: &impl TxHandle,
            ptr: Storable<InvBox<u32, ArenaAllocator>>,
        ) -> Storable<Self> {
            //ptr.check_target(target);
            unsafe {
                Storable::new(Foo {
                    x: ptr.into_inner_unchecked(),
                })
            }
        }
    }

    impl BaseType for Foo {}
    fn box_simple() {
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new().allocator();
        let obj = builder
            .build_with(|uo| Foo::new_in(&uo, InvBox::new_in(3, alloc, &uo).unwrap()))
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.raw.resolve(), 3);
    }
}
