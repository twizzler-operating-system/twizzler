pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::R_AARCH64_ABS64 as REL_SYMBOLIC;
pub use elf::abi::R_AARCH64_COPY as REL_COPY;
pub use elf::abi::R_AARCH64_GLOB_DAT as REL_GOT;
pub use elf::abi::R_AARCH64_JUMP_SLOT as REL_PLT;
pub use elf::abi::R_AARCH64_RELATIVE as REL_RELATIVE;
pub use elf::abi::R_AARCH64_TLS_DTPMOD as REL_DTPMOD;
pub use elf::abi::R_AARCH64_TLS_DTPREL as REL_DTPOFF;
pub use elf::abi::R_AARCH64_TLS_TPREL as REL_TPOFF;

pub unsafe fn get_thread_control_block<T>() -> *mut Tcb<T> {
    todo!()
}
