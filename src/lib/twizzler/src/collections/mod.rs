use crate::ptr::InvPtr;

#[repr(C)]
pub struct VectorHeader<T> {
    base: InvPtr<T>,
    len: u64,
    cap: u64,
}
