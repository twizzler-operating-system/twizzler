use core::mem::MaybeUninit;

use bitflags::bitflags;
use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::arch::syscall::raw_syscall;

bitflags! {
    #[derive(Debug)]
    pub struct GetRandomFlags: u32 {
        const NONBLOCKING = 1 << 0;
        const UNEXPECTED = 2 << 0;
    }
}

impl From<u64> for GetRandomFlags {
    fn from(value: u64) -> Self {
        match value {
            1 => GetRandomFlags::NONBLOCKING,
            0 => GetRandomFlags::empty(),
            _ => GetRandomFlags::UNEXPECTED,
        }
    }
}
impl From<u32> for GetRandomFlags {
    fn from(value: u32) -> Self {
        match value {
            1 => GetRandomFlags::NONBLOCKING,
            0 => GetRandomFlags::empty(),
            _ => GetRandomFlags::UNEXPECTED,
        }
    }
}

pub fn sys_get_random(dest: &mut [MaybeUninit<u8>], flags: GetRandomFlags) -> Result<usize> {
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
    let out = convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, twzerr);
    out
}
