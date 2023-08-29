use arm64::registers::{ELR_EL1, SP_EL0, SPSR_EL1};
use registers::interfaces::Writeable;

use twizzler_abi::upcall::UpcallFrame;

use crate::{memory::VirtAddr, syscall::SyscallContext};

use super::thread::UpcallAble;

#[derive(Default, Clone, Copy)]
#[repr(C)]
pub struct Armv8SyscallContext {
    x0: u64,
    elr: u64,
    sp: u64,
}

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
    fn create_jmp_context(target: VirtAddr, stack: VirtAddr, arg: u64) -> Self {
        Self {
            elr: target.into(),
            sp: stack.into(),
            x0: arg,
            ..Default::default()
        }
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

// TODO: does not have to be *const
#[allow(named_asm_labels)]
pub unsafe fn return_to_user(context: *const Armv8SyscallContext) -> ! {
    // set the entry point address
    ELR_EL1.set((*context).elr);
    // set the stack pointer
    SP_EL0.set((*context).sp);
    // configure the execution state for EL0:
    // - interrupts masked
    // - el0 exception level
    // - use sp_el0 stack pointer
    // - aarch64 execution state
    SPSR_EL1.write(
        SPSR_EL1::D::Masked + SPSR_EL1::A::Masked + SPSR_EL1::I::Masked
        + SPSR_EL1::F::Masked + SPSR_EL1::M::EL0t
    );

    // TODO: zero out/copy all registers
    core::arch::asm!(
        // copy argument to register x0
        "mov x0, {}",
        // return to address specified in elr_el1
        "eret",
        in(reg) (*context).x0,
        options(noreturn)
    )
}

// #[allow(unsupported_naked_functions)] // DEBUG
#[allow(named_asm_labels)]
#[naked]
pub unsafe extern "C" fn syscall_entry() -> ! {
    core::arch::asm!("nop", options(noreturn))
}
