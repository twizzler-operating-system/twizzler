use core::sync::atomic::AtomicPrimitive;

use twizzler_abi::meta::MetaInfo;
use twizzler_rt_abi::error::TwzError;

use crate::{
    mutex::LockGuard,
    obj::{Object, PageNumber, pagetables::ObjectPageTable},
};

impl Object {
    pub fn read_meta(&self) -> Option<MetaInfo> {
        todo!()
    }

    pub fn write_meta(&self, meta: MetaInfo) -> bool {
        todo!()
    }

    pub fn read_atomic<T: AtomicPrimitive>(&self, offset: usize) -> T {
        todo!()
    }

    pub fn swap_atomic<T: AtomicPrimitive>(&self, offset: usize, val: T) -> T {
        todo!()
    }

    pub fn write_at<T>(&self, val: &T, offset: usize) -> Result<(), TwzError> {
        todo!()
    }

    pub fn read_at<T>(&self, offset: usize) -> Result<T, TwzError> {
        todo!()
    }

    pub fn read_base<T>(&self) -> Result<T, TwzError> {
        todo!()
    }

    pub fn write_base<T>(&self, val: &T) -> Result<(), TwzError> {
        todo!()
    }

    pub fn write_bytes(&self, ptr: *const u8, len: usize, offset: usize) -> Result<(), TwzError> {
        todo!()
    }

    pub fn try_write_val_and_signal<T>(
        &self,
        offset: usize,
        val: T,
        wake_count: usize,
    ) -> Result<(), TwzError> {
        todo!()
    }

    pub fn ensure_in_core(
        &self,
        guard: LockGuard<'_, ObjectPageTable>,
        page: PageNumber,
        pager_was_used: &mut bool,
    ) -> LockGuard<'_, ObjectPageTable> {
        todo!()
    }
}
