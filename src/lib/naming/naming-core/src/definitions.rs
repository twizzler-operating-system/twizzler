use arrayvec::ArrayString;
use twizzler::marker::Invariant;

pub const MAX_KEY_SIZE: usize = 255;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Schema {
    pub key: ArrayString<MAX_KEY_SIZE>,
    pub val: u128,
}

unsafe impl Invariant for Schema {}
