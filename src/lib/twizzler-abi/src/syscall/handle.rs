use bitflags::bitflags;
use twizzler_rt_abi::{
    error::{ArgumentError, TwzError},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

/// Possible kernel handle types.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u64)]
pub enum HandleType {
    VmContext = 0,
    PagerQueue = 1,
}

impl TryFrom<u64> for HandleType {
    type Error = TwzError;

    fn try_from(value: u64) -> Result<Self> {
        match value {
            0 => Ok(Self::VmContext),
            1 => Ok(Self::PagerQueue),
            _ => Err(ArgumentError::InvalidArgument.into()),
        }
    }
}

bitflags! {
    /// Flags to pass to [sys_new_handle].
    pub struct NewHandleFlags: u64 {
    }
}

bitflags! {
    /// Flags to pass to [sys_unbind_handle].
    pub struct UnbindHandleFlags: u64 {
    }
}

/// Make a new handle object.
pub fn sys_new_handle(objid: ObjID, handle_type: HandleType, flags: NewHandleFlags) -> Result<u64> {
    let [hi, lo] = objid.parts();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::NewHandle,
            &[hi, lo, handle_type as u64, flags.bits()],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, twzerr)
}

/// Unbind an object from handle status.
pub fn sys_unbind_handle(objid: ObjID, flags: UnbindHandleFlags) {
    let [hi, lo] = objid.parts();
    unsafe {
        raw_syscall(Syscall::UnbindHandle, &[hi, lo, flags.bits()]);
    }
}
