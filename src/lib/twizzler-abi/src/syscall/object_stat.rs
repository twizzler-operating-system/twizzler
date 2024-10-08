use core::mem::MaybeUninit;

use num_enum::{FromPrimitive, IntoPrimitive};

use super::{convert_codes_to_result, justval, BackingType, LifetimeType, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

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
/// Possible error returns for [sys_object_stat].
pub enum ObjectStatError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// One of the arguments was invalid.
    #[error("invalid argument")]
    InvalidArgument = 1,
    /// Invalid Object ID.
    #[error("invalid ID")]
    InvalidID = 2,
}

impl core::error::Error for ObjectStatError {}

/// Information about an object, according to the local kernel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct ObjectInfo {
    /// The ID of this object.
    pub id: ObjID,
    /// The number of mappings in which this object participates.
    pub maps: usize,
    /// The number of ties to this object.
    pub ties_to: usize,
    /// The number of ties from this object.
    pub ties_from: usize,
    /// The lifetime type of this object.
    pub life: LifetimeType,
    /// The backing type of this object.
    pub backing: BackingType,
}

/// Read information about a given object.
pub fn sys_object_stat(id: ObjID) -> Result<ObjectInfo, ObjectStatError> {
    let (hi, lo) = id.split();
    let mut obj_info = MaybeUninit::<ObjectInfo>::uninit();
    let args = [
        hi,
        lo,
        &mut obj_info as *mut MaybeUninit<ObjectInfo> as usize as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectStat, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, _| unsafe { obj_info.assume_init() },
        justval,
    )
}
