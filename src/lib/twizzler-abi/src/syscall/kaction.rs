use crate::{
    arch::syscall::raw_syscall,
    kso::{KactionCmd, KactionError, KactionFlags, KactionValue},
    object::ObjID,
};

use super::{convert_codes_to_result, Syscall};

/// Execute a kaction on an object.
pub fn sys_kaction(
    cmd: KactionCmd,
    id: Option<ObjID>,
    arg: u64,
    flags: KactionFlags,
) -> Result<KactionValue, KactionError> {
    let (hi, lo) = id.map_or((0, 0), |id| id.split());
    let (code, val) =
        unsafe { raw_syscall(Syscall::Kaction, &[cmd.into(), hi, lo, arg, flags.bits()]) };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        |c, v| KactionValue::from((c, v)),
        |_, v| KactionError::from(v),
    )
}
