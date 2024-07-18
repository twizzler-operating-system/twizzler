use std::alloc::{AllocError, Layout};

use super::Allocator;
use crate::{
    object::InitializedObject,
    ptr::{GlobalPtr, InvPtr, InvPtrBuilder},
    tx::{TxHandle, TxResult},
};

#[repr(C)]
pub struct PBox<T> {
    ptr: InvPtr<T>,
}

pub struct PBoxBuilder<T> {
    inv: InvPtrBuilder<T>,
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

//#[cfg(test)]
mod test {
    use std::u32;

    use twizzler_abi::syscall::ObjectCreate;

    use super::{PBox, PBoxBuilder};
    use crate::{
        alloc::{
            arena::{Arena, ArenaManifest},
            TxAllocator,
        },
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

    #[derive(twizzler_derive::InvariantCopy, Copy, Clone)]
    #[repr(C)]
    struct Foo2 {
        x: u32,
    }

    /*
    unsafe impl twizzler::marker::Invariant for Foo {}
    unsafe impl twizzler::marker::InPlaceCtor for Foo {
        type Builder = FooBuilder;
        fn in_place_ctor<'b>(
            builder: Self::Builder,
            place: &'b mut core::mem::MaybeUninit<Self>,
            tx: impl twizzler::tx::TxHandle<'b>,
        ) -> &'b mut Self
        where
            Self: Sized,
        {
            unsafe {
                let ptr = place as *mut _ as *mut Self;
                let ptr = addr_of!((*ptr).data) as *mut core::mem::MaybeUninit<TxCell<PBox<u32>>>;
                <TxCell<PBox<u32>>>::in_place_ctor(builder.data, &mut *ptr, &tx);
            }
            unsafe {
                let ptr = place as *mut _ as *mut Self;
                let ptr = addr_of!((*ptr).data2) as *mut core::mem::MaybeUninit<TxCell<u32>>;
                <TxCell<u32>>::in_place_ctor(builder.data2, &mut *ptr, &tx);
            }
            unsafe { place.assume_init_mut() }
        }
    }
    struct FooBuilder {
        data: <TxCell<PBox<u32>> as InPlaceCtor>::Builder, // PBoxBuilder
        data2: <TxCell<u32> as InPlaceCtor>::Builder, // u32
    }
    impl Foo {
        pub fn new(
            data: <TxCell<PBox<u32>> as InPlaceCtor>::Builder,
            data2: <TxCell<u32> as InPlaceCtor>::Builder,
        ) -> FooBuilder {
            FooBuilder { data, data2 }
        }
    }

    impl BaseType for Foo {}
    impl BaseType for Foo2 {}

    fn test<'a>(alloc: Object<ArenaManifest>, tx: impl TxHandle<'a>) {
        let obj: Object<Foo> = ObjectBuilder::default()
            .construct(|_info| Ok(Foo::new(PBox::new_in(32, &alloc).unwrap(), 334)))
            .unwrap();

        let base = obj.base();
        base.data
            .set_in_place(PBox::new_in(64, &alloc).unwrap(), &tx)
            .unwrap();
        let _data = **base.data;

        base.data2.set(42, tx).unwrap();

        let o2 = ObjectBuilder::default().init(Foo2 { x: 4 }).unwrap();
        let base = o2.base();
        let _data = base.x;
    }
    */
}
