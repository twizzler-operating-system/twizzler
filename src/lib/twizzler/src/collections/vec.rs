use std::{
    alloc::{AllocError, Layout},
    mem::MaybeUninit,
    ops::RangeBounds,
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::error::{ArgumentError, ResourceError};

use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{Invariant, StoreCopy},
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut},
    tx::TxRef,
    Result,
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
    ) -> Result<RefMut<T>> {
        let place = unsafe { Ref::from_ptr(self) };
        if newcap <= self.cap {
            // TODO: shrinking.
            return Ok(unsafe { self.start.resolve().mutable() });
        }

        let new_layout = Layout::array::<T>(newcap).map_err(|_| AllocError)?;
        let old_layout = Layout::array::<T>(self.cap).map_err(|_| AllocError)?;

        let old_global = self.start.global().cast();
        let new_alloc = alloc.realloc(old_global, old_layout, new_layout.size())?;
        let new_start = InvPtr::new(place, new_alloc.cast())?;
        self.start = new_start;
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

    fn do_remove(&mut self, idx: usize) -> Result<()> {
        let mut rslice = unsafe {
            RefSliceMut::from_ref(
                self.start.resolve().mutable().cast::<u8>(),
                self.cap * size_of::<T>(),
            )
        };
        let slice = rslice.as_slice_mut();
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
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> Result<R>,
    ) -> Result<R> {
        let r = unsafe { self.start.resolve().mutable() };
        let slice = unsafe { RefSliceMut::from_ref(r, self.len) };
        f(slice.slice(range).as_slice_mut())
    }

    fn with_mut<R>(&mut self, idx: usize, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let r = unsafe { self.start.resolve().mutable() };
        let mut slice = unsafe { RefSliceMut::from_ref(r, self.len) };
        let mut item = slice.get_mut(idx).unwrap();
        f(&mut *item)
    }
}

#[derive(twizzler_derive::BaseType)]
pub struct Vec<T: Invariant, Alloc: Allocator> {
    inner: VecInner<T>,
    alloc: Alloc,
}

#[derive(Clone)]
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

//impl<T: Invariant, A: Allocator> BaseType for Vec<T, A> {}

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

    #[inline]
    pub unsafe fn get_mut(&mut self, idx: usize) -> Option<RefMut<'_, T>> {
        let mut slice = self.as_mut_slice();
        slice.get_mut(idx)
    }

    #[inline]
    pub unsafe fn get_tx(&self, idx: usize) -> Result<Option<TxRef<T>>> {
        let slice = self.as_slice();
        slice.get_ref(idx).map(|f| f.owned().into_tx()).transpose()
    }

    pub fn new_in(alloc: Alloc) -> Self {
        Self {
            inner: VecInner {
                cap: 0,
                len: 0,
                start: InvPtr::null(),
            },
            alloc,
        }
    }

    fn get_slice_grow(&mut self) -> Result<RefMut<'_, MaybeUninit<T>>> {
        let oldlen = self.inner.len;
        tracing::trace!("len: {}, cap: {}", self.inner.len, self.inner.cap);
        if self.inner.len == self.inner.cap {
            if self.inner.start.raw() as usize + size_of::<T>() * self.inner.cap
                >= MAX_SIZE - NULLPAGE_SIZE
            {
                return Err(ResourceError::OutOfMemory.into());
            }
            let newcap = std::cmp::max(self.inner.cap, 1) * 2;
            let r = self.inner.do_realloc(newcap, oldlen + 1, &self.alloc)?;
            tracing::trace!("grow {:p}", r.raw());
            Ok(Self::maybe_uninit_slice(r, newcap)
                .get_mut(oldlen)
                .unwrap()
                .owned())
        } else {
            self.inner.len += 1;
            let resptr = unsafe { self.inner.start.resolve().mutable() };
            tracing::trace!("no grow {:p}", resptr.raw());
            Ok(Self::maybe_uninit_slice(resptr, self.inner.cap)
                .get_mut(oldlen)
                .unwrap()
                .owned())
        }
    }

    fn do_push(&mut self, item: T) -> Result<()> {
        let r = self.get_slice_grow()?;
        // write item, tracking in tx
        tracing::trace!("store value: {:p}", r.raw());
        r.write(item);
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.inner.len
    }

    pub fn capacity(&self) -> usize {
        self.inner.cap
    }

    pub fn reserve(&mut self, additional: usize) -> Result<()> {
        self.inner
            .do_realloc(self.inner.cap + additional, self.inner.len, &self.alloc)?;
        Ok(())
    }

    #[inline]
    pub fn as_slice(&self) -> RefSlice<'_, T> {
        let r = unsafe { self.inner.start.resolve() };
        let slice = unsafe { RefSlice::from_ref(r, self.inner.len) };
        slice
    }

    #[inline]
    pub unsafe fn as_mut_slice(&mut self) -> RefSliceMut<'_, T> {
        let r = unsafe { self.inner.start.resolve().mutable() };
        let slice = unsafe { RefSliceMut::from_ref(r, self.inner.len) };
        slice
    }

    pub fn remove_inplace(&mut self, idx: usize) -> Result<()> {
        if idx >= self.inner.len {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.inner.with_mut(idx, |item| {
            unsafe { core::ptr::drop_in_place(item) };
            Ok(())
        })?;
        self.inner.do_remove(idx)?;
        Ok(())
    }

    pub fn truncate(&mut self, newlen: usize) -> Result<()> {
        let oldlen = self.inner.len;
        if newlen >= oldlen {
            return Ok(());
        }
        self.inner.with_mut_slice(newlen..oldlen, |slice| {
            for item in slice {
                unsafe { core::ptr::drop_in_place(item) };
            }
            Ok(())
        })?;
        self.inner.len = newlen;
        Ok(())
    }

    pub fn shrink_to_fit(&mut self) -> Result<()> {
        self.inner.cap = self.inner.len;
        // TODO: release memory
        Ok(())
    }

    pub fn with_mut_slice<R>(
        &mut self,
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> Result<R>,
    ) -> Result<R> {
        self.inner.with_mut_slice(range, f)
    }
}

impl<T: Invariant + StoreCopy, Alloc: Allocator> Vec<T, Alloc> {
    pub fn push(&mut self, item: T) -> Result<()> {
        self.do_push(item)
    }

    pub fn pop(&mut self) -> Result<Option<T>> {
        if self.inner.len == 0 {
            return Ok(None);
        }
        let new_len = self.inner.len - 1;
        let val = self
            .inner
            .with_slice(|slice| unsafe { ((&slice[new_len]) as *const T).read() });
        self.inner.do_remove(new_len)?;
        Ok(Some(val))
    }

    pub fn remove(&mut self, idx: usize) -> Result<T> {
        //let mut inner = self.inner.get()?;
        if idx >= self.inner.len {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let val = self
            .inner
            .with_slice(|slice| unsafe { ((&slice[idx]) as *const T).read() });
        self.inner.do_remove(idx)?;
        Ok(val)
    }
}

impl<T: Invariant, Alloc: Allocator + SingleObjectAllocator> Vec<T, Alloc> {
    pub fn push_inplace(&mut self, item: T) -> Result<()> {
        self.do_push(item)
    }

    fn push_ctor<F>(&mut self, ctor: F) -> Result<()>
    where
        F: FnOnce(RefMut<MaybeUninit<T>>) -> Result<RefMut<T>>,
    {
        let r = self.get_slice_grow()?;
        let _val = ctor(r)?;
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
            place: RefMut<MaybeUninit<Self>>,
            ptr: impl Into<GlobalPtr<Simple>>,
        ) -> Result<RefMut<Self>> {
            let ptr = InvPtr::new(&place, ptr)?;
            Ok(place.write(Self { ptr }))
        }
    }

    impl BaseType for Node {}
    unsafe impl Invariant for Node {}

    #[test]
    fn simple_push() {
        let vobj = ObjectBuilder::default()
            .build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))
            .unwrap();

        let mut tx = vobj.into_tx().unwrap();
        tx.base_mut().push(Simple { x: 42 }).unwrap();
        tx.base_mut().push(Simple { x: 43 }).unwrap();
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

        let mut tx = vobj.into_tx().unwrap();
        let mut base = tx.base_mut().owned();
        base.push_inplace(Node {
            ptr: InvPtr::new(&tx, simple_obj.base_ref()).unwrap(),
        })
        .unwrap();
        let vobj = tx.commit().unwrap();

        let rbase = vobj.base();
        let item = rbase.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve() }.x, 3);
    }

    #[test]
    fn vec_object() {
        let simple_obj = ObjectBuilder::default().build(Simple { x: 3 }).unwrap();
        let mut vo = VecObject::new(ObjectBuilder::default()).unwrap();
        vo.push_ctor(|place| {
            let node = Node {
                ptr: InvPtr::new(&place, simple_obj.base_ref())?,
            };
            Ok(place.write(node))
        })
        .unwrap();

        vo.push_ctor(|place| Node::new_inplace(place, simple_obj.base_ref()))
            .unwrap();

        let base = vo.object().base();
        let item = base.get(0).unwrap();
        assert_eq!(unsafe { item.ptr.resolve().x }, 3);
    }
}
