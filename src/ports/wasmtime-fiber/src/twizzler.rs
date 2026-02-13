//! Twizzler fiber implementation.
//!
//! Stack allocation: by default, uses `Vec<u8>` (like the no_std path).
//! For production use, callers should provide Twizzler-object-backed
//! stacks via `from_custom` / `RuntimeFiberStackCreator`, which use
//! the object null page as an MMU-enforced guard page.

use crate::stackswitch::*;
use crate::{RunResult, RuntimeFiberStack};
use std::boxed::Box;
use std::cell::Cell;
use std::io;
use std::ops::Range;
use std::vec::Vec;

pub type Error = io::Error;

pub struct FiberStack {
    base: BasePtr,
    len: usize,
    storage: FiberStackStorage,
}

struct BasePtr(*mut u8);

unsafe impl Send for BasePtr {}
unsafe impl Sync for BasePtr {}

enum FiberStackStorage {
    Vec(Vec<u8>),
    Unmanaged(usize),
    Custom(Box<dyn RuntimeFiberStack>),
}

const STACK_ALIGN: usize = 16;

fn align_ptr(ptr: *mut u8, len: usize, align: usize) -> (*mut u8, usize) {
    let ptr_val = ptr as usize;
    let aligned = (ptr_val + align - 1) & !(align - 1);
    let new_len = len - (aligned - ptr_val);
    (aligned as *mut u8, new_len)
}

impl FiberStack {
    pub fn new(size: usize, zeroed: bool) -> io::Result<Self> {
        let size = core::cmp::max(4096, size);
        let mut storage = Vec::new();
        storage.reserve_exact(size);
        if zeroed {
            storage.resize(size, 0);
        }
        let (base, len) = align_ptr(storage.as_mut_ptr(), size, STACK_ALIGN);
        Ok(FiberStack {
            storage: FiberStackStorage::Vec(storage),
            base: BasePtr(base),
            len,
        })
    }

    pub unsafe fn from_raw_parts(
        base: *mut u8,
        guard_size: usize,
        len: usize,
    ) -> io::Result<Self> {
        Ok(FiberStack {
            storage: FiberStackStorage::Unmanaged(guard_size),
            base: BasePtr(unsafe { base.add(guard_size) }),
            len,
        })
    }

    pub fn is_from_raw_parts(&self) -> bool {
        matches!(self.storage, FiberStackStorage::Unmanaged(_))
    }

    pub fn from_custom(custom: Box<dyn RuntimeFiberStack>) -> io::Result<Self> {
        let range = custom.range();
        let start_ptr = range.start as *mut u8;
        Ok(FiberStack {
            base: BasePtr(start_ptr),
            len: range.len(),
            storage: FiberStackStorage::Custom(custom),
        })
    }

    pub fn top(&self) -> Option<*mut u8> {
        Some(self.base.0.wrapping_byte_add(self.len))
    }

    pub fn range(&self) -> Option<Range<usize>> {
        let base = self.base.0 as usize;
        Some(base..base + self.len)
    }

    pub fn guard_range(&self) -> Option<Range<*mut u8>> {
        match &self.storage {
            FiberStackStorage::Unmanaged(guard_size) => unsafe {
                let start = self.base.0.sub(*guard_size);
                Some(start..self.base.0)
            },
            FiberStackStorage::Custom(custom) => Some(custom.guard_range()),
            FiberStackStorage::Vec(_) => None,
        }
    }
}

pub struct Fiber;

pub struct Suspend {
    top_of_stack: *mut u8,
}

extern "C" fn fiber_start<F, A, B, C>(arg0: *mut u8, top_of_stack: *mut u8)
where
    F: FnOnce(A, &mut super::Suspend<A, B, C>) -> C,
{
    unsafe {
        let inner = Suspend { top_of_stack };
        let initial = inner.take_resume::<A, B, C>();
        super::Suspend::<A, B, C>::execute(inner, initial, Box::from_raw(arg0.cast::<F>()))
    }
}

impl Fiber {
    pub fn new<F, A, B, C>(stack: &FiberStack, func: F) -> io::Result<Self>
    where
        F: FnOnce(A, &mut super::Suspend<A, B, C>) -> C,
    {
        if !SUPPORTED_ARCH {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "fibers not supported on this host architecture",
            ));
        }
        unsafe {
            let data = Box::into_raw(Box::new(func)).cast();
            wasmtime_fiber_init(stack.top().unwrap(), fiber_start::<F, A, B, C>, data);
        }
        Ok(Self)
    }

    pub(crate) fn resume<A, B, C>(&self, stack: &FiberStack, result: &Cell<RunResult<A, B, C>>) {
        unsafe {
            let addr = stack.top().unwrap().cast::<usize>().offset(-1);
            addr.write(result as *const _ as usize);

            assert!(SUPPORTED_ARCH);
            wasmtime_fiber_switch(stack.top().unwrap());

            addr.write(0);
        }
    }

    pub(crate) unsafe fn drop<A, B, C>(&mut self) {}
}

impl Suspend {
    pub(crate) fn switch<A, B, C>(&mut self, result: RunResult<A, B, C>) -> A {
        unsafe {
            (*self.result_location::<A, B, C>()).set(result);
            wasmtime_fiber_switch(self.top_of_stack);
            self.take_resume::<A, B, C>()
        }
    }

    pub(crate) fn exit<A, B, C>(&mut self, result: RunResult<A, B, C>) {
        self.switch(result);
        unreachable!()
    }

    unsafe fn take_resume<A, B, C>(&self) -> A {
        unsafe {
            match (*self.result_location::<A, B, C>()).replace(RunResult::Executing) {
                RunResult::Resuming(val) => val,
                _ => panic!("not in resuming state"),
            }
        }
    }

    unsafe fn result_location<A, B, C>(&self) -> *const Cell<RunResult<A, B, C>> {
        unsafe {
            let ret = self.top_of_stack.cast::<*const u8>().offset(-1).read();
            assert!(!ret.is_null());
            ret.cast()
        }
    }
}
