use crate::tls::{Tcb, TlsRegion};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::{
    R_X86_64_64 as REL_SYMBOLIC, R_X86_64_COPY as REL_COPY, R_X86_64_DTPMOD64 as REL_DTPMOD,
    R_X86_64_DTPOFF64 as REL_DTPOFF, R_X86_64_GLOB_DAT as REL_GOT, R_X86_64_JUMP_SLOT as REL_PLT,
    R_X86_64_RELATIVE as REL_RELATIVE, R_X86_64_TPOFF64 as REL_TPOFF,
};

/// Get a pointer to the current thread control block, using the thread pointer.
///
/// # Safety
/// The TCB must actually contain runtime data of type T, and be initialized.
pub unsafe fn get_current_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mov {}, fs:0", out(reg) val);
    val as *mut _
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.    
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        self.get_thread_pointer_value() as *mut _
    }
}
