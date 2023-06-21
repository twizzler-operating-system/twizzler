/// CPU context (register state) switching.
/// 
/// NOTE: According to section 6.1.1 of the 64-bit ARM
/// Procedure Call Standard (PCS), not all registers
/// need to be saved, only those needed for a subroutine call.
/// 
/// A full detailed explanation can be found in the
/// "Procedure Call Standard for the Arm® 64-bit Architecture (AArch64)":
///     https://github.com/ARM-software/abi-aa/releases/download/2023Q1/aapcs64.pdf

use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

use crate::{
    thread::Thread,
};

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

    pub fn set_tls(&self, _tls: u64) {
        todo!()
    }

    pub extern "C" fn arch_switch_to(&self, _old_thread: &Thread) {
        todo!()
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
