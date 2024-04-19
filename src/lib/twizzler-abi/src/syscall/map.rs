use core::mem::MaybeUninit;

use bitflags::bitflags;
use num_enum::{FromPrimitive, IntoPrimitive};

use crate::{
    arch::syscall::raw_syscall,
    object::{ObjID, Protections},
};

use super::{convert_codes_to_result, justval, Syscall};

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
/// Possible error values for [sys_object_map].
pub enum ObjectMapError {
    #[num_enum(default)]
    /// An unknown error occurred.
    #[error("unknown error")]
    Unknown = 0,
    /// The specified object was not found.
    #[error("object not found")]
    ObjectNotFound = 1,
    /// The specified slot was invalid.
    #[error("invalid slot")]
    InvalidSlot = 2,
    /// The specified protections were invalid.
    #[error("invalid protections")]
    InvalidProtections = 3,
    /// An argument was invalid.
    #[error("invalid argument")]
    InvalidArgument = 4,
}

impl core::error::Error for ObjectMapError {}

bitflags! {
    /// Flags to pass to [sys_object_map].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
/// Possible error values for [sys_object_unmap].
pub enum ObjectUnmapError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// The specified slot was invalid.
    #[error("invalid slot")]
    InvalidSlot = 1,
    /// An argument was invalid.
    #[error("invalid argument")]
    InvalidArgument = 2,
}

impl core::error::Error for ObjectUnmapError {}

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

#[derive(
    Debug,
    Copy,
    Clone,
    PartialEq,
    PartialOrd,
    Ord,
    Eq,
    FromPrimitive,
    IntoPrimitive,
    thiserror::Error,
)]
#[repr(u64)]
/// Possible error values for [sys_object_unmap].
pub enum ObjectReadMapError {
    /// An unknown error occurred.
    #[num_enum(default)]
    #[error("unknown error")]
    Unknown = 0,
    /// The specified slot was invalid.
    #[error("invalid slot")]
    InvalidSlot = 1,
    /// An argument was invalid.
    #[error("invalid argument")]
    InvalidArgument = 2,
}

impl core::error::Error for ObjectReadMapError {}

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
