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
    ptr::{GlobalPtr, InvPtr, Ref, RefMut, RefSlice, RefSliceMut, TxRef, TxRefSlice},
    util::same_object,
    Result,
};

mod vec_object;
pub use vec_object::{VecIter, VecObject};

#[cfg(test)]
mod tests;

pub struct VecInner<T: Invariant> {
    len: usize,
    cap: usize,
    start: InvPtr<T>,
}

impl<T: Invariant> VecInner<T> {
    fn resolve_start(&self) -> Ref<'_, T> {
        unsafe { self.start.resolve() }
    }

    fn resolve_start_tx(&self) -> Result<TxRef<T>> {
        let mut tx = unsafe { self.start.resolve().into_tx() }?;
        if same_object(tx.raw(), self) {
            tx.nosync();
        }
        Ok(tx)
    }

    fn do_realloc<Alloc: Allocator>(
        &mut self,
        newcap: usize,
        newlen: usize,
        alloc: &Alloc,
    ) -> Result<()> {
        let place = unsafe { Ref::from_ptr(self) };
        if newcap <= self.cap {
            // TODO: shrinking.
            return Ok(());
        }

        let new_layout = Layout::array::<T>(newcap).map_err(|_| AllocError)?;
        let old_layout = Layout::array::<T>(self.cap).map_err(|_| AllocError)?;

        let old_global = self.start.global().cast();
        let new_alloc = unsafe { alloc.realloc(old_global, old_layout, new_layout.size()) }?;
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

        Ok(())
    }

    fn do_remove(&mut self, idx: usize) -> Result<()> {
        let mut rslice = unsafe {
            TxRefSlice::from_ref(
                self.start.resolve().into_tx()?.cast::<u8>(),
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
        let r = self.resolve_start();
        let slice = unsafe { RefSlice::from_ref(r, self.len) };
        slice
    }

    fn with_slice<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        let r = self.resolve_start();
        let slice = unsafe { RefSlice::from_ref(r, self.len) };
        f(slice.as_slice())
    }

    fn with_mut_slice<R>(
        &mut self,
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> Result<R>,
    ) -> Result<R> {
        let r = self.resolve_start_tx()?;
        let slice = unsafe { TxRefSlice::from_ref(r, self.len) };
        f(slice.slice(range).as_slice_mut())
    }

    fn with_mut<R>(&mut self, idx: usize, f: impl FnOnce(&mut T) -> Result<R>) -> Result<R> {
        let r = self.resolve_start_tx()?;
        let mut slice = unsafe { TxRefSlice::from_ref(r, self.len) };
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

    unsafe fn realloc(
        &self,
        _ptr: GlobalPtr<u8>,
        _layout: Layout,
        newsize: usize,
    ) -> std::result::Result<GlobalPtr<u8>, AllocError> {
        // 1 for null page, 2 for metadata pages, 1 for base
        if newsize > MAX_SIZE - NULLPAGE_SIZE * 4 {
            return Err(std::alloc::AllocError);
        }
        let obj = twizzler_rt_abi::object::twz_rt_get_object_handle((self as *const Self).cast())
            .unwrap();
        Ok(GlobalPtr::new(obj.id(), (NULLPAGE_SIZE * 2) as u64))
    }
}

impl SingleObjectAllocator for VecObjectAlloc {}

//impl<T: Invariant, A: Allocator> BaseType for Vec<T, A> {}

impl<T: Invariant, Alloc: Allocator> Vec<T, Alloc> {
    fn maybe_uninit_slice<'a>(r: TxRef<T>, cap: usize) -> TxRefSlice<MaybeUninit<T>> {
        unsafe { TxRefSlice::from_ref(r.cast(), cap) }
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

    fn get_slice_grow(&mut self) -> Result<TxRef<MaybeUninit<T>>> {
        let oldlen = self.inner.len;
        tracing::trace!("len: {}, cap: {}", self.inner.len, self.inner.cap);
        if self.inner.len == self.inner.cap {
            if self.inner.start.raw() as usize + size_of::<T>() * self.inner.cap
                >= MAX_SIZE - NULLPAGE_SIZE
            {
                return Err(ResourceError::OutOfMemory.into());
            }
            let newcap = std::cmp::max(self.inner.cap, 1) * 2;
            self.inner.do_realloc(newcap, oldlen + 1, &self.alloc)?;
            let r = self.inner.resolve_start_tx()?;
            tracing::trace!("grow {:p}", r.raw());
            Ok(Self::maybe_uninit_slice(r, newcap)
                .get_into(oldlen)
                .unwrap())
        } else {
            self.inner.len += 1;
            let r = self.inner.resolve_start_tx()?;
            tracing::trace!("no grow {:p} {}", r.raw(), r.is_nosync());
            Ok(Self::maybe_uninit_slice(r, self.inner.cap)
                .get_into(oldlen)
                .unwrap())
        }
    }

    fn do_push(&mut self, item: T) -> Result<()> {
        let r = self.get_slice_grow()?;
        tracing::trace!("store value: {:p}", r.raw());
        let r = r.write(item)?;
        drop(r);
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
        let r = self.inner.resolve_start();
        let slice = unsafe { RefSlice::from_ref(r, self.inner.len) };
        slice
    }

    #[inline]
    pub fn as_tx_slice(&self) -> Result<TxRefSlice<T>> {
        let r = self.inner.resolve_start_tx()?;
        let slice = unsafe { TxRefSlice::from_ref(r, self.inner.len) };
        Ok(slice)
    }

    #[inline]
    pub unsafe fn as_mut_slice(&mut self) -> RefSliceMut<'_, T> {
        let r = unsafe { self.inner.start.resolve().into_mut() };
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

    pub fn with_slice<R>(&self, f: impl FnOnce(&[T]) -> R) -> R {
        self.inner.with_slice(f)
    }

    pub fn with_mut_slice<R>(
        &mut self,
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> Result<R>,
    ) -> Result<R> {
        self.inner.with_mut_slice(range, f)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) -> Result<()> {
        self.truncate(0)
    }

    pub fn swap(&mut self, a: usize, b: usize) {
        if a == b {
            return;
        }
        unsafe {
            let mut slice = self.as_mut_slice();
            let slice_mut = slice.as_slice_mut();
            slice_mut.swap(a, b);
        }
    }

    pub fn first_ref(&self) -> Option<Ref<'_, T>> {
        self.get_ref(0)
    }

    pub fn last_ref(&self) -> Option<Ref<'_, T>> {
        if self.inner.len == 0 {
            None
        } else {
            self.get_ref(self.inner.len - 1)
        }
    }

    pub fn contains(&self, item: &T) -> bool
    where
        T: PartialEq,
    {
        self.with_slice(|slice| slice.contains(item))
    }

    pub fn starts_with(&self, needle: &[T]) -> bool
    where
        T: PartialEq,
    {
        self.with_slice(|slice| slice.starts_with(needle))
    }

    pub fn ends_with(&self, needle: &[T]) -> bool
    where
        T: PartialEq,
    {
        self.with_slice(|slice| slice.ends_with(needle))
    }

    pub fn binary_search(&self, x: &T) -> std::result::Result<usize, usize>
    where
        T: Ord,
    {
        self.with_slice(|slice| slice.binary_search(x))
    }

    pub fn binary_search_by<F>(&self, f: F) -> std::result::Result<usize, usize>
    where
        F: FnMut(&T) -> std::cmp::Ordering,
    {
        self.with_slice(|slice| slice.binary_search_by(f))
    }

    pub fn reverse(&mut self) -> Result<()> {
        self.with_mut_slice(.., |slice| {
            slice.reverse();
            Ok(())
        })
    }

    pub fn sort(&mut self) -> Result<()>
    where
        T: Ord,
    {
        self.with_mut_slice(.., |slice| {
            slice.sort();
            Ok(())
        })
    }

    pub fn sort_by<F>(&mut self, compare: F) -> Result<()>
    where
        F: FnMut(&T, &T) -> std::cmp::Ordering,
    {
        self.with_mut_slice(.., |slice| {
            slice.sort_by(compare);
            Ok(())
        })
    }

    pub fn sort_unstable(&mut self) -> Result<()>
    where
        T: Ord,
    {
        self.with_mut_slice(.., |slice| {
            slice.sort_unstable();
            Ok(())
        })
    }

    pub fn sort_unstable_by<F>(&mut self, compare: F) -> Result<()>
    where
        F: FnMut(&T, &T) -> std::cmp::Ordering,
    {
        self.with_mut_slice(.., |slice| {
            slice.sort_unstable_by(compare);
            Ok(())
        })
    }

    pub fn retain<F>(&mut self, mut f: F) -> Result<()>
    where
        F: FnMut(&T) -> bool,
    {
        let mut del = 0;
        let len = self.len();

        for i in 0..len {
            let should_retain = self.with_slice(|slice| f(&slice[i - del]));
            if !should_retain {
                self.remove_inplace(i - del)?;
                del += 1;
            }
        }

        Ok(())
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
        let mut r = self.get_slice_grow()?;
        tracing::info!("run push ctor");
        let _val = ctor(r.as_mut())?;
        Ok(())
    }
}
