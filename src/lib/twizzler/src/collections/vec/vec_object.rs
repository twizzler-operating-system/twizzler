use std::{mem::MaybeUninit, ops::RangeBounds};

use twizzler_rt_abi::error::ArgumentError;

use super::{Vec, VecObjectAlloc};
use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{Ref, RefSlice},
    tx::TxRef,
};

pub struct VecObject<T: Invariant, A: Allocator> {
    obj: Object<Vec<T, A>>,
}

impl<T: Invariant, A: Allocator> Clone for VecObject<T, A> {
    fn clone(&self) -> Self {
        Self {
            obj: self.obj.clone(),
        }
    }
}

impl<T: Invariant, A: Allocator> From<Object<Vec<T, A>>> for VecObject<T, A> {
    fn from(value: Object<Vec<T, A>>) -> Self {
        Self { obj: value }
    }
}

impl<T: Invariant, A: Allocator> VecObject<T, A> {
    pub fn object(&self) -> &Object<Vec<T, A>> {
        &self.obj
    }

    pub fn into_object(self) -> Object<Vec<T, A>> {
        self.obj
    }

    pub fn iter(&self) -> VecIter<'_, T> {
        if self.len() == 0 {
            return VecIter {
                pos: 0,
                data: core::ptr::null(),
                len: 0,
                _ref: None,
            };
        }
        let base = self.object().base();
        let data = unsafe { base.inner.start.resolve().owned() };
        VecIter {
            pos: 0,
            data: data.raw(),
            len: self.len(),
            _ref: Some(data),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn len(&self) -> usize {
        self.obj.base().len()
    }

    pub fn capacity(&self) -> usize {
        self.obj.base().capacity()
    }

    pub fn reserve(&mut self, additional: usize) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.reserve(additional, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn shrink_to_fit(&mut self) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.shrink_to_fit(&tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn truncate(&mut self, len: usize) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.truncate(len, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn as_slice(&self) -> RefSlice<'_, T> {
        self.obj.base().as_slice()
    }

    pub fn slice(&self, range: impl RangeBounds<usize>) -> RefSlice<'_, T> {
        self.obj.base().as_slice().slice(range)
    }

    pub fn with_mut_slice<R>(
        &mut self,
        range: impl RangeBounds<usize>,
        f: impl FnOnce(&mut [T]) -> crate::tx::Result<R>,
    ) -> crate::tx::Result<R> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        let ret = base.with_mut_slice(range, &tx, f)?;
        self.obj = tx.commit()?;
        Ok(ret)
    }
}

impl<T: Invariant + StoreCopy, A: Allocator> VecObject<T, A> {
    pub fn push(&mut self, val: T) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.push(val, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn append(&mut self, vals: impl IntoIterator<Item = T>) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        for val in vals {
            base.push(val, &tx)?;
        }
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn pop(&mut self) -> crate::tx::Result<Option<T>> {
        if self.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.remove(self.len() - 1)?))
    }

    pub fn remove(&mut self, idx: usize) -> crate::tx::Result<T> {
        if idx >= self.len() {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        let val = base.remove(idx, &tx)?;
        self.obj = tx.commit()?;
        Ok(val)
    }

    pub fn split_off(&mut self, _point: usize) -> crate::tx::Result<Self> {
        todo!()
    }

    pub fn swap_remove(&mut self, _idx: usize) -> crate::tx::Result<T> {
        todo!()
    }
}

impl<T: Invariant> VecObject<T, VecObjectAlloc> {
    pub fn new(builder: ObjectBuilder<Vec<T, VecObjectAlloc>>) -> crate::tx::Result<Self> {
        Ok(Self {
            obj: builder.build_inplace(|tx| tx.write(Vec::new_in(VecObjectAlloc)))?,
        })
    }

    #[inline]
    pub fn get(&self, idx: usize) -> Option<&T> {
        self.object().base().get(idx)
    }

    #[inline]
    pub fn get_ref(&self, idx: usize) -> Option<Ref<'_, T>> {
        self.object().base().get_ref(idx)
    }
}

impl<T: Invariant, A: Allocator + SingleObjectAllocator> VecObject<T, A> {
    pub fn push_inplace(&mut self, val: T) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref();
        base.push_inplace(val, &tx)?;
        drop(base);
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn append_inplace(&mut self, vals: impl IntoIterator<Item = T>) -> crate::tx::Result<()> {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref();
        for val in vals {
            base.push_inplace(val, &tx)?;
        }
        drop(base);
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn push_ctor<F>(&self, ctor: F) -> crate::tx::Result<()>
    where
        F: FnOnce(TxRef<MaybeUninit<T>>) -> crate::tx::Result<TxRef<T>>,
    {
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.push_ctor(tx, ctor)
    }

    pub fn remove_inplace(&mut self, idx: usize) -> crate::tx::Result<()> {
        if idx >= self.len() {
            return Err(ArgumentError::InvalidArgument.into());
        }
        let tx = self.obj.clone().tx()?;
        let base = tx.base_ref().owned();
        base.remove_inplace(idx, &tx)?;
        self.obj = tx.commit()?;
        Ok(())
    }

    pub fn swap_remove_inplace(&mut self, _idx: usize) -> crate::tx::Result<()> {
        todo!()
    }
}

impl<T: Invariant + StoreCopy, A: Allocator> Extend<T> for VecObject<T, A> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        self.append(iter).unwrap();
    }
}

pub struct VecIter<'a, T> {
    pos: usize,
    data: *const T,
    len: usize,
    _ref: Option<Ref<'a, T>>,
}

impl<'a, T> VecIter<'a, T> {
    pub fn slice(&self) -> &'a [T] {
        unsafe { core::slice::from_raw_parts(self.data, self.len) }
    }
}

impl<'a, T: 'a> Iterator for VecIter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        let pos = self.pos;
        self.pos += 1;
        self.slice().get(pos)
    }
}

impl<'a, T: Invariant, A: Allocator> IntoIterator for &'a VecObject<T, A> {
    type Item = &'a T;

    type IntoIter = VecIter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
