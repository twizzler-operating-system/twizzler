use std::alloc::{AllocError, Layout};

use super::Allocator;
use crate::{
    marker::InPlaceCtor,
    ptr::{GlobalPtr, InvPtr, InvPtrBuilder},
    tx::TxHandle,
};

#[repr(C)]
pub struct PBox<T> {
    ptr: InvPtr<T>,
}

pub struct PBoxBuilder<T> {
    inv: InvPtrBuilder<T>,
}

unsafe impl<T> InPlaceCtor for PBox<T> {
    type Builder = PBoxBuilder<T>;

    fn in_place_ctor<'b>(
        builder: Self::Builder,
        place: &'b mut std::mem::MaybeUninit<Self>,
        tx: impl TxHandle<'b>,
    ) -> &'b mut Self
    where
        Self: Sized,
    {
        todo!()
    }
}

impl<T> std::ops::Deref for PBox<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        todo!()
    }
}

impl<T> PBox<T> {
    pub fn new_in<A: Allocator>(value: T, alloc: &A) -> Result<PBoxBuilder<T>, AllocError> {
        todo!()
    }
}

impl<T> Drop for PBox<T> {
    fn drop(&mut self) {
        todo!()
    }
}

mod test {
    use std::u32;

    use twizzler_abi::syscall::ObjectCreate;

    use super::{PBox, PBoxBuilder};
    use crate::{
        alloc::{
            arena::{Arena, ArenaManifest},
            TxAllocator,
        },
        marker::InPlaceCtor,
        object::{BaseType, ConstructorInfo, InitializedObject, Object, ObjectBuilder},
        ptr::InvPtrBuilder,
        tx::{TxCell, TxHandle},
    };

    #[derive(twizzler_derive::Invariant)]
    #[repr(C)]
    struct Foo {
        data: TxCell<PBox<u32>>,
        data2: TxCell<u32>,
    }

    /*
    impl Foo {
        fn new(d: PBoxBuilder<u32>, d2: u32) -> FooBuilder {
            FooBuilder { data: d, data2: d2 }
        }
    }

    unsafe impl InPlaceCtor for Foo {
        type Builder = InvPtrBuilder<u32>;

        fn in_place_ctor<'b>(
            builder: Self::Builder,
            place: &'b mut std::mem::MaybeUninit<Self>,
            tx: impl TxHandle<'b>,
        ) -> &'b mut Self
        where
            Self: Sized,
        {
            todo!()
        }
    }
    */

    impl BaseType for Foo {}

    fn test<'a>(alloc: Object<ArenaManifest>, tx: impl TxHandle<'a>) {
        let obj: Object<Foo> = ObjectBuilder::default()
            .construct(|_info| Foo::new(PBox::new_in(32, &alloc).unwrap(), 334))
            .unwrap();

        let base = obj.base();
        base.data
            .set_in_place(PBox::new_in(64, &alloc).unwrap(), &tx)
            .unwrap();
        let _data = **base.data;

        base.data2.set(42, tx).unwrap();
    }
}
