#![feature(fn_traits)]
#![feature(unboxed_closures)]
#![feature(tuple_trait)]
#![feature(naked_functions)]

pub use secgate_macros::*;

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
#[repr(C)]
pub enum SecGateReturn<T> {
    Success(T),
    PermissionDenied,
    CalleePanic,
}

#[repr(C)]
pub struct SecGateInfo<F> {
    imp: F,
    name: *const i8,
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

#[derive(Clone, bytemuck::Pod, Copy, Debug, bytemuck::Zeroable)]
#[repr(C)]
pub struct RawSecGateInfo {
    pub imp: usize,
    pub name: usize,
}
static_assertions::assert_eq_size!(RawSecGateInfo, SecGateInfo<&()>);
