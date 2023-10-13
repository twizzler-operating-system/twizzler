pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::R_X86_64_64 as REL_SYMBOLIC;
pub use elf::abi::R_X86_64_COPY as REL_COPY;
pub use elf::abi::R_X86_64_DTPMOD64 as REL_DTPMOD;
pub use elf::abi::R_X86_64_DTPOFF64 as REL_DTPOFF;
pub use elf::abi::R_X86_64_GLOB_DAT as REL_GOT;
pub use elf::abi::R_X86_64_JUMP_SLOT as REL_PLT;
pub use elf::abi::R_X86_64_RELATIVE as REL_RELATIVE;
pub use elf::abi::R_X86_64_TPOFF64 as REL_TPOFF;
