use crate::tls::{Tcb, TlsRegion};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::R_AARCH64_ABS64 as REL_SYMBOLIC;
pub use elf::abi::R_AARCH64_COPY as REL_COPY;
pub use elf::abi::R_AARCH64_GLOB_DAT as REL_GOT;
pub use elf::abi::R_AARCH64_JUMP_SLOT as REL_PLT;
pub use elf::abi::R_AARCH64_RELATIVE as REL_RELATIVE;
pub use elf::abi::R_AARCH64_TLS_DTPMOD as REL_DTPMOD;
pub use elf::abi::R_AARCH64_TLS_DTPREL as REL_DTPOFF;
pub use elf::abi::R_AARCH64_TLS_TPREL as REL_TPOFF;

/// Get a pointer to the current thread control block, using the thread pointer.
///
/// # Safety
/// The TCB must actually contain runtime data of type T, and be initialized.
pub unsafe fn get_current_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    val as *mut _
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.    
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        todo!()
    }
}