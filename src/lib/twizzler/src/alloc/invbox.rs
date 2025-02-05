use super::{Allocator, OwnedGlobalPtr};
use crate::{
    marker::Invariant,
    ptr::{GlobalPtr, InvPtr, Ref},
    tx::TxObject,
};

pub struct InvBox<T: Invariant, Alloc: Allocator> {
    raw: InvPtr<T>,
    alloc: Alloc,
}

impl<T: Invariant, Alloc: Allocator> InvBox<T, Alloc> {
    pub unsafe fn from_invptr(raw: InvPtr<T>, alloc: Alloc) -> Self {
        Self { raw, alloc }
    }

    pub fn new<B>(_tx: &TxObject<B>, _ogp: OwnedGlobalPtr<T, Alloc>) -> Self {
        todo!()
    }

    pub fn resolve(&self) -> Ref<'_, T> {
        unsafe { self.raw.resolve() }
    }

    pub fn global(&self) -> GlobalPtr<T> {
        self.raw.global()
    }

    pub fn as_ptr(&self) -> &InvPtr<T> {
        &self.raw
    }

    pub fn alloc(&self) -> &Alloc {
        &self.alloc
    }
}

#[cfg(kani)]
mod kani_alloc {

    use twizzler_minruntime;
    use twizzler_rt_abi::bindings::twz_rt_map_object;
    use twizzler_abi::syscall::{
        self, sys_object_create, BackingType, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags, Syscall,
    };
    use twizzler_rt_abi::object::{MapFlags, ObjectHandle};
    use super::*;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaObject},
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
    };


    fn raw_syscall_kani_stub(call: Syscall, args: &[u64]) -> (u64, u64) {

        // if core::intrinsics::unlikely(args.len() > 6) {
        //     twizzler_abi::print_err("too many arguments to raw_syscall");
        //     // crate::internal_abort();
        // }
        let a0 = *args.first().unwrap_or(&0u64);
        let a1 = *args.get(1).unwrap_or(&0u64);
        let mut a2 = *args.get(2).unwrap_or(&0u64);
        let a3 = *args.get(3).unwrap_or(&0u64);
        let a4 = *args.get(4).unwrap_or(&0u64);
        let a5 = *args.get(5).unwrap_or(&0u64);

        let mut num = call.num();
        //TODO: Skip actual inline assembly invcation and register inputs
        //TODO: Improve actual logic here

        (num,a2)
    }

    struct Foo {
        x: InvBox<u32, ArenaAllocator>,
    }
    impl BaseType for Foo {}

   
    #[kani::proof]
    #[kani::stub(twizzler_abi::arch::syscall::raw_syscall,raw_syscall_kani_stub)]
    #[kani::stub(twizzler_rt_abi::bindings::twz_rt_map_object, twizzler_minruntime::runtime::syms::twz_rt_map_object)]
    fn box_simple() {
        let val: u32 = kani::any();
        let alloc = ArenaObject::new().unwrap();
        let arena = alloc.tx().unwrap();
        let foo = arena
            .alloc(Foo {
                x: InvBox::new(&arena, arena.alloc(val).unwrap()),
            })
            .unwrap();

        let base = foo.resolve();
        assert_eq!(*base.x.resolve(), val);
    }


    fn box_simple_builder() {
        let val: u32 = kani::any();
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new().unwrap();
        let obj = builder
            .build_inplace(|tx| {
                let x = InvBox::new(&tx, alloc.alloc(val).unwrap());
                tx.write(Foo { x })
            })
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.resolve(), val);
    }
}

#[cfg(test)]
mod tests {
    use super::InvBox;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaObject},
        marker::BaseType,
        object::{ObjectBuilder, TypedObject},
    };

    struct Foo {
        x: InvBox<u32, ArenaAllocator>,
    }
    impl BaseType for Foo {}

    #[test]
    fn box_simple() {
        let alloc = ArenaObject::new().unwrap();
        let arena = alloc.tx().unwrap();
        let foo = arena
            .alloc(Foo {
                x: InvBox::new(&arena, arena.alloc(3).unwrap()),
            })
            .unwrap();

        let base = foo.resolve();
        assert_eq!(*base.x.resolve(), 3);
    }

    #[test]
    fn box_simple_builder() {
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new().unwrap();
        let obj = builder
            .build_inplace(|tx| {
                let x = InvBox::new(&tx, alloc.alloc(3).unwrap());
                tx.write(Foo { x })
            })
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.resolve(), 3);
    }
}
