use crate::object::MAX_SIZE;

pub mod syscall;
pub(crate) mod upcall;

// Max size of user addr space divided into slots of size MAX_SIZE
pub const SLOTS: usize = (1 << 47) / MAX_SIZE;

use upcall::UpcallFrame;
pub use upcall::XSAVE_LEN;

pub struct ArchRegisters {
    pub frame: UpcallFrame,
    pub fs: u32,
    pub gs: u32,
    pub es: u32,
    pub ds: u32,
    pub ss: u32,
    pub cs: u32,
}
