use crate::object::MAX_SIZE;

pub mod syscall;
pub(crate) mod upcall;

// Max size of user addr space divided into slots of size MAX_SIZE
pub const SLOTS: usize = (1 << 47) / MAX_SIZE;

pub use upcall::XSAVE_LEN;
