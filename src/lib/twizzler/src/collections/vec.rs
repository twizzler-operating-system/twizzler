use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
    ops::RangeBounds,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::error::{ArgumentError, ResourceError};

use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{BaseType, Invariant, StoreCopy},
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut},
    tx::{Result, TxCell, TxHandle, TxObject, TxRef},
};

mod vec_object;
pub use vec_object::{VecIter, VecObject};

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
        tracing::trace!(
            "set start: {:x} len {}, cap {}",
            self.start.raw(),
            self.len,
            self.cap
        );

        Ok(unsafe { new_alloc.cast::<T>().resolve().owned().mutable() })
    }

    fn do_remove(&mut self, idx: usize, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let tx = tx.as_ref();
        let mut rslice =
            unsafe { RefSliceMut::from_ref(self.start.resolve().mutable().cast::<u8>(), self.cap) };
        let slice = rslice.as_slice_mut();
        let ptr = tx
            .as_ref()
            .tx_mut(slice.as_ptr(), slice.len() * size_of::<T>())?;
        let slice = unsafe { core::slice::from_raw_parts_mut(ptr, slice.len() * size_of::<T>()) };
        let byte_idx_start = (idx + 1) * size_of::<T>();
        let byte_idx = idx * size_of::<T>();
        let byte_end = self.len * size_of::<T>();
        tracing::trace!(
            "slice byte copy: {} {} {}",
            byte_idx,
            byte_idx_start,
            byte_end
        );
        slice.copy_within(byte_idx_start..byte_end, byte_idx);
        if byte_idx_start == byte_end {
            slice[byte_idx..byte_idx_start].fill(0);
        }
        self.len -= 1;
        Ok(())
    }

    pub fn as_slice(&self) -> RefSlice<'_, T> {
        let r = unsafe { self.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.len) };
        slice
    }

    fn with_slice<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let r = unsafe { self.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.len) };
        f(slice.as_slice())
    }

    fn with_mut_slice<R>(
        &mut self,
        tx: &impl TxHandle,
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> crate::tx::Result<R>,
    ) -> crate::tx::Result<R> {
        let r = unsafe { self.start.resolve() };
        let mut slice = unsafe { RefSlice::from_ref(r, self.len).tx(range, tx)? };
        f(slice.as_slice_mut())
    }

    fn with_mut<R>(
        &mut self,
        idx: usize,
        tx: &impl TxHandle,
        f: impl FnOnce(&mut T) -> crate::tx::Result<R>,
    ) -> crate::tx::Result<R> {
        let r = unsafe { self.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.len) };
        let item = slice.get_ref(idx).unwrap();
        let mut item = item.tx(tx)?;
        f(&mut *item)
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

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        let slice = self.as_slice();
        slice.as_slice().get(idx)
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'_, T>> {
        let slice = self.as_slice();
        slice.get_ref(idx)
    }

    pub fn get_mut(
        &self,
        idx: usize,
        tx: impl AsRef<TxObject>,
    ) -> crate::tx::Result<Option<RefMut<'_, T>>> {
        let slice = self.as_slice();
        slice
            .get_ref(idx)
            .map(|f| f.owned().tx(tx.as_ref()))
            .transpose()
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
        tracing::trace!("len: {}, cap: {}", self.inner.len, self.inner.cap);
        if self.inner.len == self.inner.cap {
            if self.inner.start.raw() as usize + size_of::<T>() * self.inner.cap
                >= MAX_SIZE - NULLPAGE_SIZE
            {
                return Err(ResourceError::OutOfMemory.into());
            }
            let newcap = std::cmp::max(self.inner.cap, 1) * 2;
            let inner = self.inner.get_mut(tx.as_ref())?;
            let r = inner.do_realloc(newcap, oldlen + 1, &self.alloc, tx.as_ref())?;
            tracing::trace!("grow {:p}", r.raw());
            Ok(Self::maybe_uninit_slice(r, newcap)
                .get_mut(oldlen)
                .unwrap()
                .owned())
        } else {
            self.inner.get_mut(tx.as_ref())?.len += 1;
            let resptr = unsafe { self.inner.start.resolve().mutable() };
            tracing::trace!("no grow {:p}", resptr.raw());
            Ok(Self::maybe_uninit_slice(resptr, self.inner.cap)
                .get_mut(oldlen)
                .unwrap()
                .owned())
        }
    }

    fn do_push(&self, item: T, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let mut r = self.get_slice_grow(&tx)?;
        // write item, tracking in tx
        tracing::trace!("store value: {:p}", r.raw());
        tx.as_ref().write_uninit(&mut *r, item)?;
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.len
    }

    pub fn capacity(&self) -> usize {
        self.inner.cap
    }

    pub fn reserve(&self, additional: usize, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        inner.do_realloc(inner.cap + additional, inner.len, &self.alloc, tx.as_ref())?;
        Ok(())
    }

    #[inline]
    pub fn as_slice(&self) -> RefSlice<'_, T> {
        let r = unsafe { self.inner.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.inner.len) };
        slice
    }

    pub fn remove_inplace(&self, idx: usize, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        if idx >= inner.len {
            return Err(ArgumentError::InvalidArgument.into());
        }
        inner.with_mut(idx, tx.as_ref(), |item| {
            unsafe { core::ptr::drop_in_place(item) };
            Ok(())
        })?;
        inner.do_remove(idx, tx)?;
        Ok(())
    }

    pub fn truncate(&self, newlen: usize, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        let oldlen = inner.len;
        if newlen >= oldlen {
            return Ok(());
        }
        inner.with_mut_slice(tx.as_ref(), newlen..oldlen, |slice| {
            for item in slice {
                unsafe { core::ptr::drop_in_place(item) };
            }
            Ok(())
        })?;
        inner.len = newlen;
        Ok(())
    }

    pub fn shrink_to_fit(&self, tx: impl AsRef<TxObject>) -> crate::tx::Result<()> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        inner.cap = inner.len;
        // TODO: release memory
        Ok(())
    }

    pub fn with_mut_slice<R>(
        &self,
        range: impl RangeBounds<usize>,
        tx: impl AsRef<TxObject>,
        f: impl FnOnce(&mut [T]) -> crate::tx::Result<R>,
    ) -> crate::tx::Result<R> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        inner.with_mut_slice(tx.as_ref(), range, f)
    }
}

impl<T: Invariant + StoreCopy, Alloc: Allocator> Vec<T, Alloc> {
    pub fn push(&self, item: T, tx: impl AsRef<TxObject>) -> Result<()> {
        self.do_push(item, tx)
    }

    pub fn pop(&self, tx: impl AsRef<TxObject>) -> Result<Option<T>> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        if inner.len == 0 {
            return Ok(None);
        }
        let val = inner.with_slice(|slice| unsafe { ((&slice[inner.len - 1]) as *const T).read() });
        inner.do_remove(inner.len - 1, tx)?;
        Ok(Some(val))
    }

    pub fn remove(&self, idx: usize, tx: impl AsRef<TxObject>) -> Result<T> {
        let inner = self.inner.get_mut(tx.as_ref())?;
        if idx >= inner.len {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let val = inner.with_slice(|slice| unsafe { ((&slice[idx]) as *const T).read() });
        inner.do_remove(idx, tx)?;
        Ok(val)
    }
}

impl<T: Invariant, Alloc: Allocator + SingleObjectAllocator> Vec<T, Alloc> {
    pub fn push_inplace(&self, item: T, tx: impl AsRef<TxObject>) -> Result<()> {
        self.do_push(item, tx)
    }

    fn push_ctor<B, F>(&self, tx: TxObject<B>, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        let mut r = self.get_slice_grow(&tx)?;
        let txref = unsafe { TxRef::from_raw_parts(tx, &mut *r) };
        let val = ctor(txref)?;
        val.into_tx().commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[allow(dead_code)]
mod tests {
    use super::*;
    use crate::{
        marker::{BaseType, Invariant},
        object::{ObjectBuilder, TypedObject},
        ptr::{GlobalPtr, InvPtr},
    };

    #[derive(Copy, Clone)]
    struct Simple {
        x: u32,
    }
    unsafe impl Invariant for Simple {}

    impl BaseType for Simple {}

    struct Node {
        pub ptr: InvPtr<Simple>,
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
    fn simple_remove_vo() {
        let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
        vec_obj.push(Simple { x: 42 }).unwrap();

        let item = vec_obj.get(0).unwrap();
        assert_eq!(item.x, 42);
        let ritem = vec_obj.remove(0).unwrap();

        assert_eq!(ritem.x, 42);
    }

    #[test]
    fn multi_remove_vo() {
        let mut vec_obj = VecObject::new(ObjectBuilder::default()).unwrap();
        vec_obj.push(Simple { x: 42 }).unwrap();
        vec_obj.push(Simple { x: 43 }).unwrap();
        vec_obj.push(Simple { x: 44 }).unwrap();

        let item = vec_obj.get(0).unwrap();
        assert_eq!(item.x, 42);
        let item = vec_obj.get(1).unwrap();
        assert_eq!(item.x, 43);
        let item = vec_obj.get(2).unwrap();
        assert_eq!(item.x, 44);
        let item = vec_obj.get(3);
        assert!(item.is_none());

        let ritem = vec_obj.remove(1).unwrap();
        assert_eq!(ritem.x, 43);

        let item = vec_obj.get(0).unwrap();
        assert_eq!(item.x, 42);
        let item = vec_obj.get(1).unwrap();
        assert_eq!(item.x, 44);
        let item = vec_obj.get(2);
        assert!(item.is_none());
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

    #[test]
    fn node_push() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vobj = ObjectBuilder::<Vec<Node, VecObjectAlloc>>::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let tx = vobj.tx().unwrap();
        let base = tx.base_ref().owned();
        base.push_inplace(
            Node {
                ptr: InvPtr::new(&tx, simple_obj.base_ref()).unwrap(),
            },
            &tx,
        )
        .unwrap();
        let vobj = tx.commit().unwrap();

        let rbase = vobj.base();
        let item = rbase.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }

    #[test]
    fn vec_object() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let vo = VecObject::new(ObjectBuilder::default()).unwrap();
        vo.push_ctor(|mut place| {
            let node = Node {
                ptr: InvPtr::new(place.tx_mut(), simple_obj.base_ref())?,
            };
            place.write(node)
        })
        .unwrap();

        vo.push_ctor(|place| Node::new_inplace(place, simple_obj.base_ref()))
            .unwrap();

        let base = vo.object().base();
        let item = base.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve().x }, 3);
    }
}
