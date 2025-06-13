use std::marker::PhantomData;

use twizzler_abi::object::MAX_SIZE;
use twizzler_rt_abi::object::ObjectHandle;

use super::{GlobalPtr, Ref, RefMut};
use crate::{
    marker::{Invariant, PhantomStoreEffect},
    object::{FotEntry, RawObject},
};

#[repr(C)]
#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone, Copy)]
pub struct InvPtr<T: Invariant> {
    value: u64,
    _pse: PhantomStoreEffect,
    _pd: PhantomData<*const T>,
}

impl<T: Invariant> InvPtr<T> {
    fn get_this(this: *const Self) -> ObjectHandle {
        twizzler_rt_abi::object::twz_rt_get_object_handle(this.cast()).unwrap()
    }

    pub fn global(&self) -> GlobalPtr<T> {
        let fote = self.fot_index();
        let obj = Self::get_this(self);
        if fote == 0 {
            return GlobalPtr::new(obj.id(), self.offset());
        }
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE).unwrap();
        GlobalPtr::new(re.id(), self.offset())
    }

    #[inline(always)]
    fn local_resolve(&self) -> *const T {
        let this = self as *const Self;
        this.map_addr(|addr| (addr & !(MAX_SIZE - 1)) + self.offset() as usize)
            .cast()
    }

    #[inline]
    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        if core::intrinsics::likely(self.is_local()) {
            Ref::from_ptr(self.local_resolve())
        } else {
            self.slow_resolve()
        }
    }

    #[inline]
    pub unsafe fn resolve_mut(&self) -> RefMut<'_, T> {
        let fote = self.fot_index();
        let obj = Self::get_this(self);
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE).unwrap();
        let ptr = re
            .lea_mut(self.offset() as usize, size_of::<T>())
            .unwrap()
            .cast();
        RefMut::from_handle(re, ptr)
    }

    #[inline(never)]
    unsafe fn slow_resolve(&self) -> Ref<'_, T> {
        let fote = self.fot_index();
        let obj = Self::get_this(self);
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE).unwrap();
        let ptr = re
            .lea(self.offset() as usize, size_of::<T>())
            .unwrap()
            .cast();
        Ref::from_handle(re, ptr)
    }

    pub const fn null() -> Self {
        Self::from_raw_parts(0, 0)
    }

    pub const fn from_raw_parts(idx: u32, offset: u64) -> Self {
        Self {
            value: ((idx as u64) << 48) | offset,
            _pse: PhantomStoreEffect,
            _pd: PhantomData,
        }
    }

    #[inline(always)]
    pub const fn fot_index(&self) -> u64 {
        self.value >> 48
    }

    #[inline(always)]
    pub const fn is_local(&self) -> bool {
        self.fot_index() == 0
    }

    #[inline(always)]
    pub const fn offset(&self) -> u64 {
        self.value & ((1 << 48) - 1)
    }

    #[inline]
    pub const fn raw(&self) -> u64 {
        self.value
    }

    pub fn new(
        tx: impl Into<ObjectHandle>,
        gp: impl Into<GlobalPtr<T>>,
    ) -> crate::tx::Result<Self> {
        let gp = gp.into();
        let tx: ObjectHandle = tx.into();
        if gp.id() == tx.id() {
            return Ok(Self::from_raw_parts(0, gp.offset()));
        }
        let fote: FotEntry = gp.into();
        let fote =
            twizzler_rt_abi::object::twz_rt_insert_fot(&tx, (&fote as *const FotEntry).cast())?;
        Ok(Self::from_raw_parts(fote, gp.offset()))
    }

    pub const fn cast<U: Invariant>(self) -> InvPtr<U> {
        unsafe { InvPtr::from_raw_parts(self.fot_index() as u32, self.offset()) }
    }
    
    pub const unsafe fn add(self, count: usize) -> Self {
        self.byte_add(count * size_of::<T>())
    }

    pub const unsafe fn byte_add(self, count: usize) -> Self {
        Self::from_raw_parts(self.fot_index() as u32, self.offset() + count as u64)
    }

    pub const unsafe fn sub(self, count: usize) -> Self {
        self.byte_sub(count * size_of::<T>())
    }

    pub const unsafe fn byte_sub(self, count: usize) -> Self {
        Self::from_raw_parts(self.fot_index() as u32, self.offset() - count as u64)
    }
    
    pub const unsafe fn make_offset(self, count: isize) -> Self {
        self.make_byte_offset(count * size_of::<T>() as isize)
    }

    pub const unsafe fn make_byte_offset(self, count: isize) -> Self {
        Self::from_raw_parts(self.fot_index() as u32, (self.offset() as isize + count) as u64)
    }
}
