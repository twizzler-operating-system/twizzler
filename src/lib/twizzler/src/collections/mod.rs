use std::{
    alloc::{AllocError, Layout},
    cmp::max,
    pin::Pin,
};

use twizzler_derive::NewStorer;

use crate::{
    alloc::Allocator,
    marker::Storer,
    object::BaseType,
    ptr::{InvPtr, ResolvedPtr, ResolvedSlice},
    tx::{TxCell, TxHandle, TxResult},
};

mod vec;

#[derive(twizzler_derive::Invariant, NewStorer)]
#[repr(C)]
struct VectorInner<T> {
    ptr: InvPtr<T>,
    cap: u64,
    len: u64,
}

impl<T> Default for VectorInner<T> {
    fn default() -> Self {
        Self {
            ptr: InvPtr::null(),
            cap: 0,
            len: 0,
        }
    }
}

impl<T> VectorInner<T> {
    fn reserve<Alloc: Allocator>(
        &mut self,
        additional: u64,
        alloc: &Alloc,
    ) -> Result<(), AllocError> {
        if (self.len + additional) < self.cap {
            return Ok(());
        }

        let new_cap = max(max(self.cap * 2, self.len + additional), 8);
        let old_layout = Layout::array::<T>(self.cap as usize).map_err(|_| AllocError)?;
        let layout = Layout::array::<T>(new_cap as usize).map_err(|_| AllocError)?;
        if self.ptr.is_null() {
            self.ptr
                .set(alloc.allocate(layout)?.cast())
                .map_err(|_| AllocError)?;
            self.cap = new_cap;
            return Ok(());
        }

        let old_ptr = self.ptr.try_as_global().map_err(|_| AllocError)?;

        let new_ptr = unsafe {
            if alloc
                .resize_in_place(old_ptr.cast(), old_layout, layout.size())
                .is_ok()
            {
                self.cap = new_cap;
                return Ok(());
            }

            alloc.grow(
                old_ptr.cast(),
                old_layout,
                layout.size() - old_layout.size(),
            )
        }?;

        self.cap = new_cap;
        if self.ptr.set(new_ptr.cast()).is_err() {
            let _ = unsafe { alloc.deallocate(new_ptr, layout) };
            self.ptr = InvPtr::null();
            return Err(AllocError);
        }

        Ok(())
    }
}

#[derive(twizzler_derive::Invariant)]
#[repr(C)]
pub struct Vector<T, Alloc: Allocator> {
    inner: TxCell<VectorInner<T>>,
    alloc: Alloc,
}

impl<T, Alloc: Allocator> Vector<T, Alloc> {
    pub fn new_in(alloc: Alloc) -> Storer<Self> {
        unsafe {
            Storer::new_move(Self {
                inner: TxCell::new(VectorInner::default()),
                alloc,
            })
        }
    }

    pub fn push<'a>(&self, item: T, tx: impl TxHandle<'a>) -> TxResult<(), AllocError> {
        self.inner.try_modify(
            |inner| {
                unsafe {
                    let inner = Pin::get_unchecked_mut(inner);
                    inner.reserve(1, &self.alloc)?;
                    let slice = inner.ptr.resolve();
                    slice.into_mut().ptr().add(inner.len as usize).write(item);
                    inner.len += 1;
                }
                Ok(())
            },
            tx,
        )
    }

    pub fn pop<'a>(&self, tx: impl TxHandle<'a>) -> TxResult<Option<T>, AllocError> {
        self.inner.try_modify(
            |inner| {
                if inner.len == 0 {
                    return Ok(None);
                }
                let item = unsafe {
                    let inner = Pin::get_unchecked_mut(inner);
                    let slice = inner.ptr.resolve();
                    inner.len -= 1;
                    slice.into_mut().ptr().add(inner.len as usize).read()
                };
                Ok(Some(item))
            },
            tx,
        )
    }

    pub fn get(&self, idx: usize) -> Option<ResolvedPtr<'_, T>> {
        if idx >= self.inner.len as usize {
            return None;
        }
        let slice = unsafe {
            ResolvedSlice::from_raw_parts(
                self.inner.ptr.try_resolve().ok()?,
                self.inner.len as usize,
            )
        };
        slice.get(idx).map(|r| r)
    }
}

impl<T, Alloc: Allocator> BaseType for Vector<T, Alloc> {}

#[cfg(test)]
mod tests {
    use super::Vector;
    use crate::{
        alloc::arena::{ArenaAllocator, ArenaManifest},
        object::{InitializedObject, Object, ObjectBuilder},
        tx::UnsafeTxHandle,
    };

    fn init() -> Object<Vector<i32, ArenaAllocator>> {
        let arena = ObjectBuilder::default()
            .construct(|_| ArenaManifest::new())
            .unwrap();
        let alloc = ArenaAllocator::new(arena.base_ref());
        ObjectBuilder::default()
            .construct(|_| Vector::new_in(alloc))
            .unwrap()
    }

    #[test]
    fn test_push() {
        let v = init();
        let tx = unsafe { UnsafeTxHandle::new() };
        v.base().push(32, tx).unwrap();
        v.base().push(42, tx).unwrap();

        let get0 = v.base().get(0).map(|rp| *rp);
        let get1 = v.base().get(1).map(|rp| *rp);

        assert_eq!(get0, Some(32));
        assert_eq!(get1, Some(42));
    }

    #[test]
    fn test_pop() {
        let v = init();
        let tx = unsafe { UnsafeTxHandle::new() };
        v.base().push(32, tx).unwrap();
        v.base().push(42, tx).unwrap();

        let pop1 = v.base().pop(tx).unwrap();
        let pop0 = v.base().pop(tx).unwrap();

        assert_eq!(pop0, Some(32));
        assert_eq!(pop1, Some(42));
    }
}
