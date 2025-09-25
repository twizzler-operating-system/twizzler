use twizzler_rt_abi::Result;

use super::{convert_codes_to_result, twzerr, Syscall};
use crate::{
    arch::syscall::raw_syscall,
    kso::{KactionCmd, KactionFlags, KactionGenericCmd, KactionValue},
    object::ObjID,
};

/// Execute a kaction on an object.
pub fn sys_kaction(
    cmd: KactionCmd,
    id: Option<ObjID>,
    arg: u64,
    arg2: u64,
    flags: KactionFlags,
) -> Result<KactionValue> {
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
        twzerr,
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

pub fn map_pages(
    id: Option<ObjID>,
    obj_start: u64,
    phys_start: u64,
    len: u32,
    uc: bool,
) -> Result<()> {
    let mut arg = phys_start;
    if uc {
        arg |= 1 << 63;
    }
    let arg2 = (len as u64) | (obj_start & 0xFFFFFFFF) << 32;
    let obj_start_hi = (obj_start >> 32) & 0xFFFF;
    sys_kaction(
        KactionCmd::Generic(KactionGenericCmd::MapPhys(obj_start_hi as u16)),
        id,
        arg,
        arg2,
        KactionFlags::empty(),
    )
    .map(|_| ())
}
