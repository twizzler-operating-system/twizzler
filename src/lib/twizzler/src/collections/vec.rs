use std::{
    alloc::Layout,
    mem::MaybeUninit,
    ops::{Index, IndexMut},
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{BaseType, Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{InvPtr, Ref, RefMut, RefSlice, RefSliceMut},
    tx::{Result, TxCell, TxHandle, TxObject, TxRef},
};

pub struct VecInner<T: Invariant> {
    len: usize,
    cap: usize,
    start: InvPtr<T>,
}

pub struct Vec<T: Invariant, Alloc: Allocator> {
    inner: TxCell<VecInner<T>>,
    alloc: Alloc,
}

pub struct VecObjectAlloc;

impl Allocator for VecObjectAlloc {
    fn alloc(
        &self,
        layout: Layout,
    ) -> std::result::Result<crate::ptr::GlobalPtr<u8>, std::alloc::AllocError> {
        todo!()
    }

    unsafe fn dealloc(&self, ptr: crate::ptr::GlobalPtr<u8>, layout: Layout) {
        todo!()
    }
}

impl SingleObjectAllocator for VecObjectAlloc {}

impl<T: Invariant, A: Allocator> BaseType for Vec<T, A> {}

impl<T: Invariant, Alloc: Allocator> Vec<T, Alloc> {
    pub fn get<'a>(&'a self, idx: usize) -> Option<Ref<'a, T>> {
        let r = unsafe { self.inner.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.inner.len) };
        slice.get(idx)
    }

    pub fn new_in(alloc: Alloc) -> Self {
        Self {
            inner: TxCell::new(VecInner {
                cap: 0,
                len: 0,
                start: todo!(),
            }),
            alloc,
        }
    }
}

impl<T: Invariant, Alloc: Allocator + SingleObjectAllocator> Vec<T, Alloc> {
    pub fn push(&self, item: T, tx: &impl TxHandle) -> Result<()> {
        if self.inner.len == self.inner.cap {
            if self.inner.start.raw() as usize + size_of::<T>() * self.inner.cap
                >= MAX_SIZE - NULLPAGE_SIZE
            {
                return Err(crate::tx::TxError::Exhausted);
            }
            let ptr = self.alloc.alloc(todo!())?;
            let inner = self.inner.get_mut(tx)?;
            // update inner.ptr
            inner.start.set(ptr, tx)?;
            todo!();
            //inner.cap += 1;
        }
        // get start slice
        let mut r = unsafe {
            RefSliceMut::from_ref(
                self.inner
                    .start
                    .resolve()
                    .cast::<MaybeUninit<T>>()
                    .mutable(),
                self.inner.cap,
            )
        };
        // write item, tracking in tx
        tx.write_uninit(&mut r[self.inner.len], item)?;
        self.inner.get_mut(tx)?.len += 1;
        Ok(())
    }

    // todo: only if storecopy
    pub fn pop(&self, tx: &impl TxHandle) -> Result<T> {
        todo!()
    }

    pub fn push_inplace<B, F>(&self, tx: &TxObject<B>, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        todo!()
    }
}

mod tests {
    use super::*;
    use crate::{
        marker::{BaseType, Invariant},
        object::TypedObject,
        ptr::{GlobalPtr, InvPtr},
    };

    #[derive(Copy, Clone)]
    struct Simple {
        x: u32,
    }
    unsafe impl Invariant for Simple {}

    impl BaseType for Simple {}

    struct Node {
        ptr: InvPtr<Simple>,
    }

    impl Node {
        pub fn new_inplace(
            place: TxRef<MaybeUninit<Self>>,
            ptr: impl Into<GlobalPtr<Simple>>,
        ) -> crate::tx::Result<TxRef<Self>> {
            todo!()
        }
    }

    impl BaseType for Node {}
    unsafe impl Invariant for Node {}

    fn simple_push() {
        let vobj = ObjectBuilder::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let tx = vobj.tx().unwrap();
        tx.base().push(Simple { x: 42 }, &tx).unwrap();
        let vobj = tx.commit().unwrap();

        let base = vobj.base();
        let item = base.get(0).unwrap();
        assert_eq!(item.x, 42);
    }

    fn node_push() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vobj = ObjectBuilder::<Vec<Node, VecObjectAlloc>>::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let tx = vobj.tx().unwrap();
        tx.base()
            .push_inplace(&tx, |place| {
                let node = Node {
                    ptr: InvPtr::new(place.tx(), simple_obj.base())?,
                };
                place.write(node)
            })
            .unwrap();
        let vobj = tx.commit().unwrap();

        let base = vobj.base();
        let item = base.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }

    fn vec_object() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vobj = ObjectBuilder::<Vec<Node, VecObjectAlloc>>::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();
        let vo = VecObject { obj: vobj };
        vo.push_inplace(|place| {
            let node = Node {
                ptr: InvPtr::new(place.tx(), simple_obj.base())?,
            };
            place.write(node)
        })
        .unwrap();

        vo.push_inplace(|place| Node::new_inplace(place, simple_obj.base()))
            .unwrap();

        let base = vo.obj.base();
        let item = base.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }
}

struct VecObject<T: Invariant, A: Allocator> {
    obj: Object<Vec<T, A>>,
}

impl<T: Invariant, A: Allocator + SingleObjectAllocator> VecObject<T, A> {
    pub fn push(&mut self, val: T) -> crate::tx::Result<()> {
        todo!()
    }

    pub fn push_inplace<F>(&self, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        let tx = self.obj.clone().tx()?;
        let base = tx.base();
        let r = base.push_inplace(&tx, ctor);
        tx.commit()?;
        r
    }

    // todo: only if storecopy
    pub fn pop(&mut self) -> crate::tx::Result<T> {
        todo!()
    }
}
