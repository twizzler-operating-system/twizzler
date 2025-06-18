use std::{mem::MaybeUninit, ops::RangeBounds};

use twizzler_rt_abi::error::ArgumentError;

use super::{Vec, VecObjectAlloc};
use crate::{
    alloc::{Allocator, SingleObjectAllocator},
    marker::{Invariant, StoreCopy},
    object::{Object, ObjectBuilder, TypedObject},
    ptr::{Ref, RefMut, RefSlice},
    Result,
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

    pub fn reserve(&mut self, additional: usize) -> Result<()> {
        self.obj.with_tx(|tx| {
            let mut base = tx.base_mut();
            base.reserve(additional)
        })
    }

    pub fn shrink_to_fit(&mut self) -> Result<()> {
        self.obj.with_tx(|tx| tx.base_mut().shrink_to_fit())
    }

    pub fn truncate(&mut self, len: usize) -> Result<()> {
        self.obj.with_tx(|tx| tx.base_mut().truncate(len))
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
        f: impl FnOnce(&mut [T]) -> Result<R>,
    ) -> Result<R> {
        self.obj
            .with_tx(|tx| tx.base_mut().with_mut_slice(range, f))
    }
}

impl<T: Invariant + StoreCopy, A: Allocator> VecObject<T, A> {
    pub fn push(&mut self, val: T) -> Result<()> {
        self.obj.with_tx(|tx| tx.base_mut().push(val))
    }

    pub fn append(&mut self, vals: impl IntoIterator<Item = T>) -> Result<()> {
        self.obj.with_tx(|tx| {
            for val in vals {
                tx.base_mut().push(val)?;
            }
            Ok(())
        })
    }

    pub fn pop(&mut self) -> Result<Option<T>> {
        if self.is_empty() {
            return Ok(None);
        }
        Ok(Some(self.remove(self.len() - 1)?))
    }

    pub fn remove(&mut self, idx: usize) -> Result<T> {
        if idx >= self.len() {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.obj.with_tx(|tx| tx.base_mut().remove(idx))
    }

    pub fn split_off(&mut self, _point: usize) -> Result<Self> {
        todo!()
    }

    pub fn swap_remove(&mut self, _idx: usize) -> Result<T> {
        todo!()
    }
}

impl<T: Invariant> VecObject<T, VecObjectAlloc> {
    pub fn new(builder: ObjectBuilder<Vec<T, VecObjectAlloc>>) -> Result<Self> {
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
    pub fn push_inplace(&mut self, val: T) -> Result<()> {
        self.obj.with_tx(|tx| tx.base_mut().push_inplace(val))
    }

    pub fn append_inplace(&mut self, vals: impl IntoIterator<Item = T>) -> Result<()> {
        self.obj.with_tx(|tx| {
            for val in vals {
                tx.base_mut().push_inplace(val)?;
            }
            Ok(())
        })
    }

    pub fn push_ctor<F>(&mut self, ctor: F) -> Result<()>
    where
        F: FnOnce(RefMut<MaybeUninit<T>>) -> Result<RefMut<T>>,
    {
        self.obj.with_tx(|tx| tx.base_mut().push_ctor(ctor))
    }

    pub fn remove_inplace(&mut self, idx: usize) -> Result<()> {
        if idx >= self.len() {
            return Err(ArgumentError::InvalidArgument.into());
        }
        self.obj.with_tx(|tx| tx.base_mut().remove_inplace(idx))
    }

    pub fn swap_remove_inplace(&mut self, _idx: usize) -> Result<()> {
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
