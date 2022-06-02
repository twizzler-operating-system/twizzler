use core::{fmt, mem::MaybeUninit};

use crate::{arch::syscall::raw_syscall, object::ObjID};

use super::{convert_codes_to_result, justval, BackingType, LifetimeType, Syscall};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_unmap].
pub enum ObjectStatError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The specified ID was invalid.
    InvalidID = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
}

impl ObjectStatError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidID => "invalid ID",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ObjectStatError> for u64 {
    fn from(x: ObjectStatError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectStatError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidID,
            2 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectStatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectStatError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

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
