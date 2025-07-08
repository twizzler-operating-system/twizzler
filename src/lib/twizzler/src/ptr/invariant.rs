use std::marker::PhantomData;

use twizzler_abi::object::MAX_SIZE;
use twizzler_rt_abi::{
    error::TwzError,
    object::{MapFlags, ObjectHandle},
};

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

unsafe impl<T: Invariant> Invariant for InvPtr<T> {}

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
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE, MapFlags::READ)
            .unwrap();
        GlobalPtr::new(re.id(), self.offset())
    }

    #[inline(always)]
    fn local_resolve(&self) -> *mut T {
        let this = self as *const Self as *mut Self;
        this.map_addr(|addr| (addr & !(MAX_SIZE - 1)) + self.offset() as usize)
            .cast()
    }

    #[inline]
    pub unsafe fn resolve(&self) -> Ref<'_, T> {
        if core::intrinsics::likely(self.is_local()) {
            return Ref::from_ptr(self.local_resolve());
        }
        let res = self
            .slow_resolve(MapFlags::READ | MapFlags::INDIRECT)
            .expect("failed to resolve ptr");
        if let Some(re) = res.1 {
            Ref::from_handle(re, res.0)
        } else {
            Ref::from_ptr(res.0)
        }
    }

    #[inline]
    pub unsafe fn resolve_mut(&self) -> RefMut<'_, T> {
        if core::intrinsics::likely(self.is_local()) {
            return RefMut::from_ptr(self.local_resolve());
        }
        let res = self
            .slow_resolve(MapFlags::WRITE | MapFlags::READ | MapFlags::PERSIST)
            .expect("failed to resolve ptr");
        if let Some(re) = res.1 {
            RefMut::from_handle(re, res.0)
        } else {
            RefMut::from_ptr(res.0)
        }
    }

    #[inline(never)]
    unsafe fn slow_resolve(
        &self,
        flags: MapFlags,
    ) -> Result<(*mut T, Option<ObjectHandle>), TwzError> {
        let fote = self.fot_index();
        let res: *mut u8 = twizzler_rt_abi::object::twz_rt_resolve_fot_local(
            self as *const Self as *mut u8,
            fote,
            MAX_SIZE,
            flags,
        );
        if !res.is_null() {
            return Ok((res.add(self.offset() as usize).cast(), None));
        }

        let obj = Self::get_this(self);
        let re = twizzler_rt_abi::object::twz_rt_resolve_fot(&obj, fote, MAX_SIZE, flags).unwrap();
        let ptr = re
            .lea_mut(self.offset() as usize, size_of::<T>())
            .unwrap()
            .cast();
        Ok((ptr, Some(re)))
    }

    pub const fn null() -> Self {
        Self::from_raw_parts(0, 0)
    }

    pub fn is_null(&self) -> bool {
        self.offset() == 0
    }

    pub fn from_raw_parts(idx: u32, offset: u64) -> Self {
        Self {
            value: ((idx as u64) << 48) | offset,
            _pse: PhantomStoreEffect,
            _pd: PhantomData,
        }
    }

    pub fn set(&mut self, gp: impl Into<GlobalPtr<T>>) -> crate::Result<()> {
        let tx = Self::get_this(self);
        *self = Self::new(tx, gp)?;
        Ok(())
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

    pub fn new(tx: impl AsRef<ObjectHandle>, gp: impl Into<GlobalPtr<T>>) -> crate::Result<Self> {
        let gp = gp.into();
        let tx = tx.as_ref();
        if gp.id() == tx.id() {
            return Ok(Self::from_raw_parts(0, gp.offset()));
        }
        let fote: FotEntry = gp.into();
        let fote =
            twizzler_rt_abi::object::twz_rt_insert_fot(&tx, (&fote as *const FotEntry).cast())?;
        Ok(Self::from_raw_parts(fote, gp.offset()))
    }
}
