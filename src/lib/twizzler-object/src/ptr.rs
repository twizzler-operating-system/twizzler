use std::{marker::PhantomData, sync::Arc};

use crate::{cell::TxCell, slot::Slot, Object};

#[repr(transparent)]
pub struct InvPtr<T> {
    raw: TxCell<u64>,
    _pd: PhantomData<T>,
}

impl<T> !Unpin for InvPtr<T> {}

impl<T> Object<T> {
    pub fn raw_lea<P>(&self, off: usize) -> *const P {
        let start = self.slot.vaddr_start();
        unsafe { ((start + off) as *const P).as_ref().unwrap() }
    }

    pub fn raw_lea_mut<P>(&self, off: usize) -> *mut P {
        let start = self.slot.vaddr_start();
        unsafe { ((start + off) as *mut P).as_mut().unwrap() }
    }

    pub(crate) fn get_fot_id<Target>(&self, fote: usize) -> &Object<Target> {
        todo!()
    }

    pub(crate) fn ptr_lea<'a, Target>(&'a self, fote: usize, offset: usize) -> EffAddr<'a, Target> {
        todo!()
    }
}

pub struct EffAddr<'a, T> {
    ptr: &'a T,
    obj: Arc<Slot>,
}
