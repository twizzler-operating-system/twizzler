use core::mem::MaybeUninit;

use bitflags::bitflags;
use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{
    arch::syscall::raw_syscall,
    object::{ObjID, Protections},
};

bitflags! {
    /// Flags to pass to [sys_object_map].
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    pub struct MapFlags: u32 {
    }
}

impl From<twizzler_rt_abi::object::MapFlags> for MapFlags {
    fn from(_: twizzler_rt_abi::object::MapFlags) -> Self {
        Self::empty()
    }
}

/// Map an object into the address space with the specified protections.
pub fn sys_object_map(
    handle: Option<ObjID>,
    id: ObjID,
    slot: usize,
    prot: Protections,
    flags: MapFlags,
) -> Result<usize> {
    let [hi, lo] = id.parts();
    let args = [
        hi,
        lo,
        slot as u64,
        prot.bits() as u64,
        flags.bits() as u64,
        &handle as *const Option<ObjID> as usize as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectMap, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, v| v as usize, twzerr)
}

bitflags! {
    /// Flags to pass to [sys_object_unmap].
    pub struct UnmapFlags: u32 {
    }
}

/// Unmaps an object from the address space specified by `handle` (or the current address space if
/// none is specified).
pub fn sys_object_unmap(handle: Option<ObjID>, slot: usize, flags: UnmapFlags) -> Result<()> {
    let [hi, lo] = handle.unwrap_or_else(|| ObjID::new(0)).parts();
    let args = [hi, lo, slot as u64, flags.bits() as u64];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectUnmap, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
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
pub fn sys_object_read_map(handle: Option<ObjID>, slot: usize) -> Result<MapInfo> {
    let [hi, lo] = handle.unwrap_or_else(|| ObjID::new(0)).parts();
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
        twzerr,
    )
}
