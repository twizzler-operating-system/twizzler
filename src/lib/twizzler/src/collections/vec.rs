use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{BaseType, Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut},
    tx::{Result, TxCell, TxHandle, TxObject, TxRef},
};

pub struct VecInner<T: Invariant> {
    len: usize,
    cap: usize,
    start: InvPtr<T>,
}

impl<T: Invariant> VecInner<T> {
    fn do_realloc<Alloc: Allocator>(
        &mut self,
        newcap: usize,
        newlen: usize,
        alloc: &Alloc,
        tx: &TxObject<()>,
    ) -> crate::tx::Result<RefMut<T>> {
        if newcap <= self.cap {
            // TODO: shrinking.
            return Ok(unsafe { self.start.resolve().mutable() });
        }

        let new_layout = Layout::array::<T>(newcap).map_err(|_| AllocError)?;
        let old_layout = Layout::array::<T>(self.cap).map_err(|_| AllocError)?;

        let old_global = self.start.global().cast();
        let new_alloc = alloc.realloc_tx(old_global, old_layout, new_layout.size(), tx)?;

        self.start = InvPtr::new(tx, new_alloc.cast())?;
        self.cap = newcap;
        self.len = newlen;

        Ok(unsafe { new_alloc.cast::<T>().resolve().owned().mutable() })
    }
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
        // 1 for null page, 2 for metadata pages, 1 for base
        if layout.size() > MAX_SIZE - NULLPAGE_SIZE * 4 {
            return Err(std::alloc::AllocError);
        }
        let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast())
            .unwrap();
        Ok(GlobalPtr::new(obj.id(), (NULLPAGE_SIZE * 2) as u64))
    }

    unsafe fn dealloc(&self, _ptr: crate::ptr::GlobalPtr<u8>, _layout: Layout) {}
}

impl SingleObjectAllocator for VecObjectAlloc {}

impl<T: Invariant, A: Allocator> BaseType for Vec<T, A> {}

impl<T: Invariant, Alloc: Allocator> Vec<T, Alloc> {
    fn maybe_uninit_slice<'a>(r: RefMut<'a, T>, cap: usize) -> RefSliceMut<'a, MaybeUninit<T>> {
        unsafe { RefSliceMut::from_ref(r.cast(), cap) }
    }

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
                start: InvPtr::null(),
            }),
            alloc,
        }
    }

    fn get_slice_grow(
        &self,
        tx: impl AsRef<TxObject>,
    ) -> crate::tx::Result<RefMut<'_, MaybeUninit<T>>> {
        let oldlen = self.inner.len;
        if self.inner.len == self.inner.cap {
            if self.inner.start.raw() as usize + size_of::<T>() * self.inner.cap
                >= MAX_SIZE - NULLPAGE_SIZE
            {
                return Err(crate::tx::TxError::Exhausted);
            }
            let newcap = std::cmp::max(self.inner.cap, 1) * 2;
            let inner = self.inner.get_mut(tx.as_ref())?;
            let r = inner.do_realloc(newcap, oldlen + 1, &self.alloc, tx.as_ref())?;
            Ok(Self::maybe_uninit_slice(r, newcap)
                .get_mut(oldlen)
                .unwrap()
                .owned())
        } else {
            self.inner.get_mut(tx.as_ref())?.len += 1;
            Ok(Self::maybe_uninit_slice(
                unsafe { self.inner.start.resolve().mutable() },
                self.inner.cap,
            )
            .get_mut(oldlen)
            .unwrap()
            .owned())
        }
    }

    fn do_push(&self, item: T, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let mut r = self.get_slice_grow(&tx)?;
        // write item, tracking in tx
        tx.as_ref().write_uninit(&mut *r, item)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.len
    }
}

impl<T: Invariant + StoreCopy, Alloc: Allocator> Vec<T, Alloc> {
    pub fn push_sc(&self, item: T, tx: impl AsRef<TxObject>) -> Result<()> {
        self.do_push(item, tx)
    }

    pub fn pop(&self, tx: &impl TxHandle) -> Result<T> {
        todo!()
    }
}

impl<T: Invariant, Alloc: Allocator + SingleObjectAllocator> Vec<T, Alloc> {
    pub fn push(&self, item: T, tx: impl AsRef<TxObject>) -> Result<()> {
        self.do_push(item, tx)
    }

    fn push_inplace<B, F>(&self, tx: TxObject<B>, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        let mut r = self.get_slice_grow(&tx)?;
        let txref = unsafe { TxRef::new(tx, &mut *r) };
        let val = ctor(txref)?;
        val.into_tx().commit()?;
        Ok(())
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
            mut place: TxRef<MaybeUninit<Self>>,
            ptr: impl Into<GlobalPtr<Simple>>,
        ) -> crate::tx::Result<TxRef<Self>> {
            let ptr = InvPtr::new(place.tx_mut(), ptr)?;
            place.write(Self { ptr })
        }
    }

    impl BaseType for Node {}
    unsafe impl Invariant for Node {}

    #[test]
    fn simple_push() {
        let vobj = ObjectBuilder::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let tx = vobj.tx().unwrap();
        tx.base().push(Simple { x: 42 }, &tx).unwrap();
        tx.base().push(Simple { x: 43 }, &tx).unwrap();
        let vobj = tx.commit().unwrap();

        let base = vobj.base();
        assert_eq!(base.len(), 2);
        let item = base.get(0).unwrap();
        assert_eq!(item.x, 42);
        let item2 = base.get(1).unwrap();
        assert_eq!(item2.x, 43);
    }

    #[test]
    fn simple_push_vo() {
        let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
        vec_obj.push(Simple { x: 42 }).unwrap();

        let item = vec_obj.get(0).unwrap();
        assert_eq!(item.x, 42);
    }

    #[test]
    fn many_push_vo() {
        let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
        for i in 0..100 {
            vec_obj.push(Simple { x: i * i }).unwrap();
        }

        for i in 0..100 {
            let item = vec_obj.get(i as usize).unwrap();
            assert_eq!(item.x, i * i);
        }
    }

    //#[test]
    fn node_push() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vobj = ObjectBuilder::<Vec<Node, VecObjectAlloc>>::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let tx = vobj.tx().unwrap();
        let base = tx.base().owned();
        base.push(
            Node {
                ptr: InvPtr::new(&tx, simple_obj.base()).unwrap(),
            },
            &tx,
        )
        .unwrap();
        let vobj = tx.commit().unwrap();

        let rbase = vobj.base();
        let item = rbase.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }

    //#[test]
    fn vec_object() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vo = VecObject::new(ObjectBuilder::default()).unwrap();
        vo.push_tx(|mut place| {
            let node = Node {
                ptr: InvPtr::new(place.tx_mut(), simple_obj.base())?,
            };
            place.write(node)
        })
        .unwrap();

        vo.push_tx(|place| Node::new_inplace(place, simple_obj.base()))
            .unwrap();

        let base = vo.obj.base();
        let item = base.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }
}

struct VecObject<T: Invariant, A: Allocator> {
    obj: Object<Vec<T, A>>,
}

impl<T: Invariant + StoreCopy, A: Allocator> VecObject<T, A> {
    pub fn push_sc(&mut self, val: T) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base().owned();
        base.push_sc(val, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn pop(&mut self) -> crate::tx::Result<T> {
        todo!()
    }
}

impl<T: Invariant> VecObject<T, VecObjectAlloc> {
    pub fn new(builder: ObjectBuilder<Vec<T, VecObjectAlloc>>) -> Result<Self> {
        Ok(Self {
            obj: builder.build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))?,
        })
    }

    pub fn get(&self, idx: usize) -> Option<Ref<'_, T>> {
        // TODO: inefficient
        self.obj.base().get(idx).map(|r| r.owned())
    }
}

impl<T: Invariant, A: Allocator + SingleObjectAllocator> VecObject<T, A> {
    pub fn push(&mut self, val: T) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base().owned();
        base.push(val, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn push_tx<F>(&self, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        let tx = self.obj.clone().tx()?;
        let base = tx.base().owned();
        base.push_inplace(tx, ctor)
    }
}
