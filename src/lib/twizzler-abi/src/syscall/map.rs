
use core::fmt;

use bitflags::bitflags;

use crate::{object::{ObjID, Protections}, arch::syscall::raw_syscall};

use super::{Syscall, justval, convert_codes_to_result};
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_map].
pub enum ObjectMapError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The specified object was not found.
    ObjectNotFound = 1,
    /// The specified slot was invalid.
    InvalidSlot = 2,
    /// The specified protections were invalid.
    InvalidProtections = 3,
    /// An argument was invalid.
    InvalidArgument = 4,
}

impl ObjectMapError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidProtections => "invalid protections",
            Self::InvalidSlot => "invalid slot",
            Self::ObjectNotFound => "object was not found",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ObjectMapError> for u64 {
    fn from(x: ObjectMapError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectMapError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::ObjectNotFound,
            2 => Self::InvalidSlot,
            3 => Self::InvalidProtections,
            4 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectMapError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_object_map].
    pub struct MapFlags: u32 {
    }
}

/// Map an object into the address space with the specified protections.
pub fn sys_object_map(
    handle: Option<ObjID>,
    id: ObjID,
    slot: usize,
    prot: Protections,
    flags: MapFlags,
) -> Result<usize, ObjectMapError> {
    let (hi, lo) = id.split();
    let args = [
        hi,
        lo,
        slot as u64,
        prot.bits() as u64,
        flags.bits() as u64,
        &handle as *const Option<ObjID> as usize as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectMap, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, justval)
}
