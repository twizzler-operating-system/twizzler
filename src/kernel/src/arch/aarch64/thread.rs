/// CPU context (register state) switching.
/// 
/// NOTE: According to section 6.1.1 of the 64-bit ARM
/// Procedure Call Standard (PCS), not all registers
/// need to be saved, only those needed for a subroutine call.
/// 
/// A full detailed explanation can be found in the
/// "Procedure Call Standard for the Arm® 64-bit Architecture (AArch64)":
///     https://github.com/ARM-software/abi-aa/releases/download/2023Q1/aapcs64.pdf

use arm64::registers::TPIDR_EL0;
use registers::interfaces::Writeable;

use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

use crate::thread::Thread;

use super::{exception::ExceptionContext, syscall::Armv8SyscallContext};

#[derive(Copy, Clone)]
pub enum Registers {
    None,
    Syscall(*mut Armv8SyscallContext, Armv8SyscallContext),
    Interrupt(*mut ExceptionContext, ExceptionContext),
}

/// Registers that need to be saved between context switches.
/// 
/// According to section 6.1.1, we only need to preserve
/// registers x19-x30 and the stack pointer (sp).
#[derive(Default)]
struct RegisterContext {
    x19: u64,
    x20: u64,
    x21: u64,
    x22: u64,
    x23: u64,
    x24: u64,
    x25: u64,
    x26: u64,
    x27: u64,
    x28: u64,
    x29: u64,
    // x30 aka the link register
    lr: u64,
    sp: u64,
}

// arch specific thread state
#[repr(align(64))]
pub struct ArchThread {
    context: RegisterContext,
}

unsafe impl Sync for ArchThread {}
unsafe impl Send for ArchThread {}

impl ArchThread {
    pub fn new() -> Self {
        Self { 
            context: RegisterContext::default() 
        }
    }
}

impl Default for ArchThread {
    fn default() -> Self {
        Self::new()
    }
}

pub trait UpcallAble {
    fn set_upcall(&mut self, _target: usize, _frame: u64, _info: u64, _stack: u64);
    fn get_stack_top(&self) -> u64;
}

pub fn set_upcall<T: UpcallAble + Copy>(_regs: &mut T, _target: usize, _info: UpcallInfo)
where
    UpcallFrame: From<T>,
{
    todo!()
}

impl Thread {
    pub fn arch_queue_upcall(&self, _target: super::address::VirtAddr, _info: UpcallInfo) {
        todo!()
    }

    pub fn set_entry_registers(&self, _regs: Registers) {
        todo!()
    }

    pub fn set_tls(&self, tls: u64) {
        // TODO: save TLS pointer in thread state, and track in context switches
        TPIDR_EL0.set(tls);
    }

    /// Architechture specific CPU context switch.
    /// 
    /// On 64-bit ARM systems, we only need to save a few registers
    /// then switch thread stacks before changing control flow.
    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        // The switch (1) saves registers x19-x30 and the stack pointer (sp)
        // onto the current thread's context save area (old_thread).
        // According to the 64-bit ARM PCS, this amount of context is fine.
        // Other registers are either caller saved, or pushed onto 
        // the stack when taking an exception. 
        // Then we (2) restore the registes from the next thread's (self) context
        // save area, (3) switch stacks, (4) and return control by returning
        // to the address in the link register (x30).
        unsafe {
            let current: *mut u64 = core::intrinsics::transmute(&old_thread.arch.context);
            let next: *const u64 = core::intrinsics::transmute(&self.arch.context);
            core::arch::asm!(
                // (1) save current thread's registers
                "stp x19, x20, [x11, #16 * 0]",
                "stp x21, x22, [x11, #16 * 1]",
                "stp x23, x24, [x11, #16 * 2]",
                "stp x25, x26, [x11, #16 * 3]",
                "stp x27, x28, [x11, #16 * 4]",
                // save the fp (x29) and the lr (x30)
                "stp x29, x30, [x11, #16 * 5]",
                // save stack pointer
                "mov x15, sp",
                "str x15, [x11, #16 * 6]",
                // (2) restore next thread's regs
                "ldp x19, x20, [x10, #16 * 0]",
                "ldp x21, x22, [x10, #16 * 1]",
                "ldp x23, x24, [x10, #16 * 2]",
                "ldp x25, x26, [x10, #16 * 3]",
                "ldp x27, x28, [x10, #16 * 4]",
                // restore the fp (x29) and the lr (x30)
                "ldp x29, x30, [x10, #16 * 5]",
                // (3) switch thread stacks
                "ldr x15, [x10, #16 * 6]",
                "mov sp, x15",
                // (4) execution resumes in the address
                // pointed to by the link register (x30)
                "ret",
                // assign inputs to temporary registers
                in("x11") current,
                in("x10") next,
                // TODO: test if we need this ...
                options(noreturn)
            );
        }
    }

    // this does not need to be pub, might not needed for aarch64
    pub unsafe fn init_va(&mut self, _jmptarget: u64) {
        todo!()
    }

    pub unsafe fn init(&mut self, entry: extern "C" fn()) {
        let stack = self.kernel_stack.as_ptr() as *mut u64;
        // set the stack pointer as the last thing context (x30 + 1)
        self.arch.context.sp = stack as u64;
        // set the link register as the second to last entry (x30)
        self.arch.context.lr = entry as u64;
    }
}
