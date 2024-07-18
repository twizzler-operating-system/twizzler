#[allow(unused_imports)]
use crate::upcall::{UpcallData, UpcallInfo};

/// Arch-specific frame info for upcall.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct UpcallFrame {
    // general purpose registers
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,

    // callee-saved registers
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,

    /// link register
    pub x29: u64,
    /// frame pointer (i.e., x30)
    pub fp: u64,
    /// The stack pointer, depending on the context where the exception
    /// occurred, this is either sp_el0 or sp_el1
    pub sp: u64,
    /// The program counter. The address where the exception occurred (i.e., ELR_EL1)
    pub pc: u64,
    /// The state of the processor (SPSR_EL1). Determines execution environment (e.g., interrupts)
    pub spsr: u64,
    // Thread local storage for user space
    pub tpidr: u64,
    pub tpidrro: u64,

    // security context
    pub prior_ctx: crate::object::ObjID,
}

impl UpcallFrame {
    /// Get the instruction pointer of the frame.
    pub fn ip(&self) -> usize {
        self.pc as usize
    }

    /// Get the stack pointer of the frame.
    pub fn sp(&self) -> usize {
        self.sp as usize
    }

    /// Get the base pointer of the frame.
    pub fn bp(&self) -> usize {
        self.fp as usize
    }
}

#[no_mangle]
#[cfg(feature = "runtime")]
pub(crate) unsafe extern "C" fn upcall_entry2(
    frame: *mut UpcallFrame,
    data: *const UpcallData,
) -> ! {
    use crate::runtime::__twz_get_runtime;

    crate::runtime::upcall::upcall_rust_entry(&*frame, &*data);
    let runtime = __twz_get_runtime();
    runtime.abort()
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn upcall_entry(
    frame: *mut UpcallFrame,
    data: *const UpcallData,
) -> ! {
    core::arch::asm!(
        "b upcall_entry2",
        in("x0") frame,
        in("x1") data,
        options(noreturn)
    );
}
