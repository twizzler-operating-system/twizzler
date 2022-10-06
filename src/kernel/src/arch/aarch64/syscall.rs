use twizzler_abi::upcall::UpcallFrame;

use crate::{memory::VirtAddr, syscall::SyscallContext};

use super::thread::UpcallAble;

#[derive(Default, Clone, Copy)]
#[repr(C)]
pub struct Armv8SyscallContext;

impl From<Armv8SyscallContext> for UpcallFrame {
    fn from(_int: Armv8SyscallContext) -> Self {
        todo!()
    }
}

impl UpcallAble for Armv8SyscallContext {
    fn set_upcall(&mut self, _target: usize, _frame: u64, _info: u64, _stack: u64) {
        todo!()
    }

    fn get_stack_top(&self) -> u64 {
        todo!()
    }
}

impl SyscallContext for Armv8SyscallContext {
    fn create_jmp_context(_target: VirtAddr, _stack: VirtAddr, _arg: u64) -> Self {
        todo!()
    }

    fn num(&self) -> usize {
        todo!()
    }
    fn arg0<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn arg1<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn arg2<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn arg3<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn arg4<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn arg5<T: From<u64>>(&self) -> T {
        todo!()
    }
    fn pc(&self) -> VirtAddr {
        todo!()
    }

    fn set_return_values<R1, R2>(&mut self, _ret0: R1, _ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>,
    {
        todo!()
    }
}

#[allow(named_asm_labels)]
pub unsafe fn return_to_user(_context: *const Armv8SyscallContext) -> ! {
    todo!()
}

#[allow(unsupported_naked_functions)] // DEBUG
#[allow(named_asm_labels)]
#[naked]
pub unsafe extern "C" fn syscall_entry() -> ! {
    todo!()
}
