use core::mem::MaybeUninit;

use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, BackingType, LifetimeType, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

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
pub fn sys_object_stat(id: ObjID) -> Result<ObjectInfo> {
    let [hi, lo] = id.parts();
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
        twzerr,
    )
}
