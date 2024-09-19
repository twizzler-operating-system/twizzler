use std::{
    marker::PhantomData,
    mem::{align_of, size_of},
    ptr::{addr_of, addr_of_mut},
};

use twizzler_abi::object::{MAX_SIZE, NULLPAGE_SIZE};

use crate::{
    marker::{CopyStorable, Invariant},
    tx::{TxCell, TxError, TxHandle, TxResult},
};

#[derive(crate::BaseType, crate::Invariant)]
#[repr(C)]
pub struct VectorBase<T: Invariant> {
    len: TxCell<u64>,
    max: Option<u64>,
    _pd: PhantomData<T>,
}

impl<T: Invariant> Default for VectorBase<T> {
    fn default() -> Self {
        Self {
            len: TxCell::new(0),
            max: None,
            _pd: PhantomData,
        }
    }
}

impl<T: Invariant> VectorBase<T> {
    fn array_offset(&self) -> usize {
        const MIN_ALIGN: usize = 16;
        let end = unsafe { addr_of!(*self).add(1) };
        let align_offset = end.align_offset(std::cmp::max(align_of::<T>(), MIN_ALIGN));
        align_offset + size_of::<Self>()
    }

    fn array_start(&self) -> *const T {
        unsafe { addr_of!(*self).byte_add(self.array_offset()).cast() }
    }

    pub fn max(&self) -> usize {
        self.max
            .map(|max| max as usize)
            .unwrap_or(MAX_SIZE - (NULLPAGE_SIZE * 8 + self.array_offset()) / size_of::<T>())
    }

    pub fn push_tx<'a>(&'a self, item: T, tx: impl TxHandle<'a>) -> Result<(), TxError>
    where
        T: CopyStorable,
    {
        self.len.try_modify(
            |mut len| {
                if *len as usize >= self.max() {
                    return Err(TxError::Abort(()));
                }

                unsafe {
                    let ptr = self.array_start().add(*len as usize);
                    let ptr = tx.tx_mut(ptr)?;
                    ptr.write(item);
                }

                *len += 1;

                Ok(())
            },
            &tx,
        )
    }

    pub fn get(&self, idx: usize) -> Option<&T> {
        if idx as u64 >= *self.len {
            return None;
        }
        unsafe {
            let ptr = self.array_start().add(idx);
            Some(ptr.as_ref().unwrap())
        }
    }
}

mod test {
    use super::VectorBase;
    use crate::object::{InitializedObject, ObjectBuilder};

    #[test]
    fn test() {
        let obj = ObjectBuilder::default()
            .init(VectorBase::<u32>::default())
            .unwrap();

        obj.tx(|tx| obj.base().push_tx(42, tx)).unwrap();
        let v = obj.base().get(0).cloned();
        assert_eq!(v, Some(42));
    }
}
