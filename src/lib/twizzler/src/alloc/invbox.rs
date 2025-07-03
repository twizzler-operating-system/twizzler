use twizzler_rt_abi::object::ObjectHandle;

use super::{Allocator, OwnedGlobalPtr};
use crate::{
    marker::Invariant,
    ptr::{GlobalPtr, InvPtr, Ref},
};

pub struct InvBox<T: Invariant, Alloc: Allocator> {
    raw: InvPtr<T>,
    alloc: Alloc,
}

impl<T: Invariant, Alloc: Allocator> InvBox<T, Alloc> {
    pub unsafe fn from_invptr(raw: InvPtr<T>, alloc: Alloc) -> Self {
        Self { raw, alloc }
    }

    pub fn new_in(tx: impl AsRef<ObjectHandle>, val: T, alloc: Alloc) -> crate::Result<Self> {
        let p = alloc.alloc_with(|r| Ok(r.write(val)))?;
        let ogp = unsafe { OwnedGlobalPtr::from_global(p.cast(), alloc) };
        Self::from_in(tx, ogp)
    }

    pub fn from_in(
        tx: impl AsRef<ObjectHandle>,
        ogp: OwnedGlobalPtr<T, Alloc>,
    ) -> crate::Result<Self> {
        let raw = InvPtr::new(tx, ogp.global())?;
        Ok(Self {
            raw,
            alloc: ogp.allocator().clone(),
        })
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

#[cfg(test)]
mod tests {
    use twizzler_derive::BaseType;

    use super::InvBox;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaObject},
        object::{ObjectBuilder, TypedObject},
    };

    #[derive(BaseType)]
    struct Foo {
        x: InvBox<u32, ArenaAllocator>,
    }

    #[test]
    fn box_simple() {
        let arena = ArenaObject::new(ObjectBuilder::default()).unwrap();
        let alloc = arena.allocator();
        let mut tx = arena.into_tx().unwrap();
        let foo = tx
            .alloc(Foo {
                x: InvBox::new_in(&tx, 3, alloc).unwrap(),
            })
            .unwrap();

        let base = foo.resolve();
        assert_eq!(*base.x.resolve(), 3);
    }

    #[test]
    fn box_alloc_builder() {
        let alloc = ArenaObject::new(ObjectBuilder::default()).unwrap();
        let foo = alloc
            .alloc_inplace(|tx| {
                let foo = Foo {
                    x: InvBox::new_in(&tx, 3, alloc.allocator()).unwrap(),
                };
                Ok(tx.write(foo))
            })
            .unwrap();
        let foo = foo.resolve();
        assert_eq!(*foo.x.resolve(), 3);
    }

    #[test]
    fn box_simple_builder() {
        let builder = ObjectBuilder::<Foo>::default();
        let alloc = ArenaObject::new(ObjectBuilder::default()).unwrap();
        let obj = builder
            .build_inplace(|tx| {
                let x = InvBox::new_in(&tx, 3, alloc.allocator()).unwrap();
                tx.write(Foo { x })
            })
            .unwrap();
        let base = obj.base();
        assert_eq!(*base.x.resolve(), 3);
        // Do this multiple times to check that the cache works as well.
        assert_eq!(*base.x.resolve(), 3);
        assert_eq!(*base.x.resolve(), 3);
        assert_eq!(*base.x.resolve(), 3);
    }
}
