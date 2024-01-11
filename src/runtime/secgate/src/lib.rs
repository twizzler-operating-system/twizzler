#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]
#![feature(naked_functions)]

use std::{marker::Tuple, mem::MaybeUninit};

pub use secgate_macros::*;

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub enum SecGateReturn<T> {
    Success(T),
    PermissionDenied,
    CalleePanic,
    NoReturnValue,
}

#[repr(C)]
pub struct SecGateInfo<F> {
    pub imp: F,
    pub name: *const i8,
}

unsafe impl<F: Send> Send for SecGateInfo<F> {}
unsafe impl<F: Sync> Sync for SecGateInfo<F> {}

impl<F> SecGateInfo<F> {
    pub const fn new(imp: F, name: &'static std::ffi::CStr) -> Self {
        Self {
            imp,
            name: name.as_ptr(),
        }
    }
}
pub const SECGATE_TRAMPOLINE_ALIGN: usize = 0x10;

pub type RawSecGateInfo = SecGateInfo<usize>;
static_assertions::assert_eq_size!(RawSecGateInfo, SecGateInfo<&fn()>);

#[derive(Clone, Copy)]
#[repr(C)]
pub struct Arguments<Args: Tuple> {
    args: Args,
}

impl<Args: Tuple + Copy> Arguments<Args> {
    pub fn new(args: Args) -> Self {
        Self { args }
    }

    pub fn into_inner(self) -> Args {
        self.args
    }
}

#[derive(Copy)]
#[repr(C)]
pub struct Return<T> {
    isset: bool,
    ret: MaybeUninit<T>,
}

impl<T: Copy> Clone for Return<T> {
    fn clone(&self) -> Self {
        Self {
            isset: self.isset,
            ret: if self.isset {
                MaybeUninit::new(unsafe { *self.ret.assume_init_ref() })
            } else {
                MaybeUninit::uninit()
            },
        }
    }
}

impl<T> Return<T> {
    pub fn new(ret: T) -> Self {
        Self {
            isset: true,
            ret: MaybeUninit::new(ret),
        }
    }

    pub fn into_inner(self) -> Option<T> {
        if self.isset {
            Some(unsafe { self.ret.assume_init() })
        } else {
            None
        }
    }

    pub fn new_uninit() -> Self {
        Self {
            isset: false,
            ret: MaybeUninit::uninit(),
        }
    }

    pub fn set(&mut self, val: T) {
        self.ret.write(val);
        self.isset = true;
    }
}
