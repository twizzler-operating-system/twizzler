use core::{fmt, mem::MaybeUninit};

use bitflags::bitflags;

use crate::{
    arch::syscall::raw_syscall,
    object::{ObjID, Protections},
};

use super::{convert_codes_to_result, justval, Syscall};
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

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_unmap].
pub enum ObjectUnmapError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The specified slot was invalid.
    InvalidSlot = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
}

impl ObjectUnmapError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidSlot => "invalid slot",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ObjectUnmapError> for u64 {
    fn from(x: ObjectUnmapError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectUnmapError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidSlot,
            2 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectUnmapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectUnmapError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

bitflags! {
    /// Flags to pass to [sys_object_unmap].
    pub struct UnmapFlags: u32 {
    }
}

/// Unmaps an object from the address space specified by `handle` (or the current address space if
/// none is specified).
pub fn sys_object_unmap(
    handle: Option<ObjID>,
    slot: usize,
    flags: UnmapFlags,
) -> Result<(), ObjectUnmapError> {
    let (hi, lo) = handle.unwrap_or_else(|| 0.into()).split();
    let args = [hi, lo, slot as u64, flags.bits() as u64];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectUnmap, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), justval)
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq)]
#[repr(u32)]
/// Possible error values for [sys_object_unmap].
pub enum ObjectReadMapError {
    /// An unknown error occurred.
    Unknown = 0,
    /// The specified slot was invalid.
    InvalidSlot = 1,
    /// An argument was invalid.
    InvalidArgument = 2,
}

impl ObjectReadMapError {
    fn as_str(&self) -> &str {
        match self {
            Self::Unknown => "an unknown error occurred",
            Self::InvalidSlot => "invalid slot",
            Self::InvalidArgument => "invalid argument",
        }
    }
}

impl From<ObjectReadMapError> for u64 {
    fn from(x: ObjectReadMapError) -> u64 {
        x as u64
    }
}

impl From<u64> for ObjectReadMapError {
    fn from(x: u64) -> Self {
        match x {
            1 => Self::InvalidSlot,
            2 => Self::InvalidArgument,
            _ => Self::Unknown,
        }
    }
}

impl fmt::Display for ObjectReadMapError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ObjectReadMapError {
    fn description(&self) -> &str {
        self.as_str()
    }
}

/// Information about an object mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(C)]
pub struct MapInfo {
    /// The mapped object ID.
    pub id: ObjID,
    /// The protections of the mapping.
    pub prot: Protections,
    /// The slot.
    pub slot: usize,
    /// The mapping flags.
    pub flags: MapFlags,
}

/// Reads the map information about a given slot in the address space specified by `handle` (or
/// current address space if none is specified).
pub fn sys_object_read_map(
    handle: Option<ObjID>,
    slot: usize,
) -> Result<MapInfo, ObjectReadMapError> {
    let (hi, lo) = handle.unwrap_or_else(|| 0.into()).split();
    let mut map_info = MaybeUninit::<MapInfo>::uninit();
    let args = [
        hi,
        lo,
        slot as u64,
        &mut map_info as *mut MaybeUninit<MapInfo> as usize as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectReadMap, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c != 0,
        |_, _| unsafe { map_info.assume_init() },
        justval,
    )
}
