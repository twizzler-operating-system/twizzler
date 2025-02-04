use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, justval, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};
#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    IntoPrimitive,
    FromPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error values for [sys_new_handle].
pub enum NewHandleError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// The specified object is already a handle.
    #[error("object is already a handle")]
    AlreadyHandle = 2,
    /// The specified object was not found.
    #[error("object not found")]
    NotFound = 3,
    /// The specified handle type is already saturated.
    #[error("handle type cannot be used again")]
    HandleSaturated = 4,
}

impl core::error::Error for NewHandleError {}

/// Possible kernel handle types.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[cfg_attr(kani, derive(kani::Arbitrary))]
#[repr(u64)]
pub enum HandleType {
    VmContext = 0,
    PagerQueue = 1,
}

impl TryFrom<u64> for HandleType {
    type Error = NewHandleError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::VmContext),
            1 => Ok(Self::PagerQueue),
            _ => Err(NewHandleError::InvalidArgument),
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
pub fn sys_new_handle(
    objid: ObjID,
    handle_type: HandleType,
    flags: NewHandleFlags,
) -> Result<u64, NewHandleError> {
    let [hi, lo] = objid.parts();
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::NewHandle,
            &[hi, lo, handle_type as u64, flags.bits()],
        )
    };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v, justval)
}

/// Unbind an object from handle status.
pub fn sys_unbind_handle(objid: ObjID, flags: UnbindHandleFlags) {
    let [hi, lo] = objid.parts();
    unsafe {
        raw_syscall(Syscall::UnbindHandle, &[hi, lo, flags.bits()]);
    }
}

#[cfg(kani)]
mod syscall_test {
    use super::*;

    #[kani::proof]
    fn test_handle_type_try_from_valid() {
        assert_eq!(HandleType::try_from(0), Ok(HandleType::VmContext));
        assert_eq!(HandleType::try_from(1), Ok(HandleType::PagerQueue));
    }

    #[kani::proof]
    fn test_handle_type_try_from_invalid() {
        let val: u64 = kani::any();
        kani::assume(val > 1);
        assert_eq!(HandleType::try_from(val), Err(NewHandleError::InvalidArgument));
    }

    #[kani::proof]
    fn test_handle_flip_flop() {
        let original: HandleType = kani::any();
        let converted: u64 = original as u64;
        let back: Result<HandleType, _> = HandleType::try_from(converted);

        assert_eq!(back, Ok(original));
    }

}
