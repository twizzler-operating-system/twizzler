pub use twizzler_rt_abi::object::{
    BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags,
    ObjectSource,
};
use twizzler_rt_abi::{
    bindings::{object_source, object_tie},
    Result,
};

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

/// Create an object, returning either its ID or an error.
pub fn sys_object_create(
    create: ObjectCreate,
    sources: &[object_source],
    ties: &[object_tie],
) -> Result<ObjID> {
    let args = [
        &create as *const ObjectCreate as u64,
        sources.as_ptr() as u64,
        sources.len() as u64,
        ties.as_ptr() as u64,
        ties.len() as u64,
    ];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCreate, &args) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        |x, y| crate::object::ObjID::from_parts([x, y]),
        twzerr,
    )
}

/// Copy ranges into an object that already exists, or zero ranges within it.
///
/// Each source with a non-zero id copies `len` bytes from `src_start` in that object to
/// `dest_start` in `dest`. A source with id 0 instead zeroes `[dest_start, dest_start + len)`,
/// releasing the frames under whole pages the range covers -- the way to hand memory back that a
/// live object otherwise has none, since the range reads zero again on the next touch.
///
/// No range may reach the object's meta page, and an object may not copy from itself.
pub fn sys_object_copy(dest: ObjID, sources: &[object_source]) -> Result<()> {
    let [hi, lo] = dest.parts();
    let args = [hi, lo, sources.as_ptr() as u64, sources.len() as u64];
    let (code, val) = unsafe { raw_syscall(Syscall::ObjectCopy, &args) };
    convert_codes_to_result(code, val, |c, _| c != 0, |_, _| (), twzerr)
}
