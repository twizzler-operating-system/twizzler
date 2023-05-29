use core::sync::atomic::AtomicU64;

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

// arch specific thread state
#[repr(align(64))]
pub struct ArchThread {
    pub user_fs: AtomicU64, // placeholder, x86 specific
}

unsafe impl Sync for ArchThread {}
unsafe impl Send for ArchThread {}

impl ArchThread {
    pub fn new() -> Self {
        todo!()
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

    pub unsafe fn init_va(&mut self, _jmptarget: u64) {
        todo!()
    }

    pub unsafe fn init(&mut self, _f: extern "C" fn()) {
        todo!()
    }
}
