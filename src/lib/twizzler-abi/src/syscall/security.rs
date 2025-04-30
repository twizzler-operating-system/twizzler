use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{arch::syscall::raw_syscall, object::ObjID};

/// Attach to a given security context.
pub fn sys_sctx_attach(id: ObjID) -> Result<()> {
    let args = [id.parts()[0], id.parts()[1], 0, 0, 0];
    let (code, val) = unsafe { raw_syscall(Syscall::SctxAttach, &args) };
    convert_codes_to_result(code, val, |c, _| c == 1, |_, _| (), twzerr)
}
