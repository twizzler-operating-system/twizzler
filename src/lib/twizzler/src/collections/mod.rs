use std::alloc::{AllocError, Layout};

use crate::{
    alloc::{arena::ArenaManifest, Allocator},
    object::{BaseType, ImmutableObject, InitializedObject, MutableObject, Object},
    ptr::{InvPtr, InvPtrBuilder, InvSlice},
    tx::{TxCell, TxError, TxHandle, TxResult},
};

struct VectorInner<T> {
    ptr: InvPtr<T>,
    cap: u64,
    len: u64,
}

#[repr(C)]
pub struct VectorHeader<T, Alloc: Allocator = Object<ArenaManifest>> {
    inner: TxCell<VectorInner<T>>,
    alloc: Alloc,
}

impl<T> BaseType for VectorHeader<T> {}

trait ArrayObject<T> {
    fn get(&self, idx: usize) -> Option<&T>;
    fn set(&self, idx: usize, val: T);
    fn push(&self, val: T);
    fn pop(&self) -> Option<T>;
}

impl<T> ArrayObject<T> for Object<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        todo!()
    }

    fn push(&self, val: T) {
        todo!()
    }

    fn pop(&self) -> Option<T> {
        todo!()
    }
}

impl<T> Object<VectorHeader<T>> {
    /*
    fn set_new_base<'a>(&self, layout: Layout, tx: impl TxHandle<'a>) -> TxResult<()> {
        let base = self.base();
        let ptr = base
            .alloc
            .allocate(layout)
            .map_err(|_| TxError::Exhausted)?;
        // TODO: ensure lifetime safety, somehow?
        base.inner
            .get_mut(&tx)?
            .ptr
            .set(unsafe { InvPtrBuilder::from_global(ptr.cast()) });
        Ok::<_, _>(())
    }
    */
}

impl<T> ArrayObject<T> for MutableObject<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        todo!()
    }

    fn push(&self, val: T) {
        todo!()
    }

    fn pop(&self) -> Option<T> {
        todo!()
    }
}

impl<T> ArrayObject<T> for ImmutableObject<VectorHeader<T>> {
    fn get(&self, idx: usize) -> Option<&T> {
        todo!()
    }

    fn set(&self, idx: usize, val: T) {
        panic!("cannot write to immutable object")
    }

    fn push(&self, val: T) {
        panic!("cannot write to immutable object")
    }

    fn pop(&self) -> Option<T> {
        panic!("cannot write to immutable object")
    }
}
