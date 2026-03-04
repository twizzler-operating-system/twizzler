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
