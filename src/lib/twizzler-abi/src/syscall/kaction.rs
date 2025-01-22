use super::{convert_codes_to_result, Syscall};
use crate::{
    arch::syscall::raw_syscall,
    kso::{KactionCmd, KactionError, KactionFlags, KactionValue},
    object::ObjID,
};

/// Execute a kaction on an object.
pub fn sys_kaction(
    cmd: KactionCmd,
    id: Option<ObjID>,
    arg: u64,
    arg2: u64,
    flags: KactionFlags,
) -> Result<KactionValue, KactionError> {
    let [hi, lo] = id.map_or([0, 0], |id| id.parts());
    let (code, val) = unsafe {
        raw_syscall(
            Syscall::Kaction,
            &[cmd.into(), hi, lo, arg, flags.bits(), arg2],
        )
    };
    convert_codes_to_result(
        code,
        val,
        |c, _| c == 0,
        |c, v| KactionValue::from((c, v)),
        |_, v| KactionError::from(v),
    )
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, PartialOrd, Ord, Eq)]
#[repr(C)]
pub struct PinnedPage {
    phys: u64,
}

impl PinnedPage {
    pub fn new(phys: u64) -> Self {
        Self { phys }
    }

    pub fn physical_address(&self) -> u64 {
        self.phys
    }
}
