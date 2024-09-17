use core::mem::MaybeUninit;

use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, Syscall};
use crate::arch::syscall::raw_syscall;

bitflags! {
    pub struct GetRandomFlags: u32 {
        const NONBLOCKING = 1 << 0;
        const UNEXPECTED = 2 << 0;
    }
}

impl From<u64> for GetRandomFlags {
    fn from(value: u64) -> Self {
        match value {
            1 => GetRandomFlags::NONBLOCKING,
            _ => GetRandomFlags::UNEXPECTED,
        }
    }
}

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    Hash,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error returns for [sys_read_clock_info].
pub enum GetRandomError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("Random is not seeded yet and the NONBLOCKING flag was passed in.")]
    Unseeded = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
}

impl core::error::Error for GetRandomError {}

pub fn sys_get_random(
    dest: &mut [MaybeUninit<u8>],
    flags: GetRandomFlags,
) -> Result<usize, GetRandomError> {
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::GetRandom,
            &[
                dest.as_mut_ptr() as u64,
                dest.len() as u64,
                flags.bits() as u64,
            ],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, |_, v| v.into())
}
