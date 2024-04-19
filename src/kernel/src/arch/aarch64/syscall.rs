/// System call handling.
///
/// The registers used for system call arguments and return values
/// are chosen based on information in the 64-bit ARM PCS.
///
/// "Procedure Call Standard for the Arm® 64-bit Architecture (AArch64)":
///     https://github.com/ARM-software/abi-aa/releases/download/2023Q1/aapcs64.pdf

use arm64::registers::{ELR_EL1, SP_EL0, SPSR_EL1};
use registers::interfaces::Writeable;

use twizzler_abi::upcall::UpcallFrame;

use crate::{memory::VirtAddr, syscall::SyscallContext};

use super::{thread::UpcallAble, exception::ExceptionContext};

/// The register state needed to transition between kernel and user.
///
/// According to the ARM PCS Section 6, arguments/return values are
/// passed in via registers x0-x7
#[derive(Default, Clone, Copy)]
#[repr(C)]
pub struct Armv8SyscallContext {
    x0: u64,
    x1: u64,
    x2: u64,
    x3: u64,
    x4: u64,
    x5: u64,
    x6: u64,
    x7: u64,
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

// Arguments 0-5 are passed in via registers x0-x5,
// the syscall number is passed in register x6,
// and the return values are passed in via x6/x7
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
        self.x6 as usize
    }
    fn arg0<T: From<u64>>(&self) -> T {
        T::from(self.x0)
    }
    fn arg1<T: From<u64>>(&self) -> T {
        T::from(self.x1)
    }
    fn arg2<T: From<u64>>(&self) -> T {
        T::from(self.x2)
    }
    fn arg3<T: From<u64>>(&self) -> T {
        T::from(self.x3)
    }
    fn arg4<T: From<u64>>(&self) -> T {
        T::from(self.x4)
    }
    fn arg5<T: From<u64>>(&self) -> T {
        T::from(self.x5)
    }
    fn pc(&self) -> VirtAddr {
        VirtAddr::new(self.elr).unwrap()
    }

    fn set_return_values<R1, R2>(&mut self, ret0: R1, ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>,
    {
        self.x6 = u64::from(ret0);
        self.x7 = u64::from(ret1);
    }
}

#[allow(named_asm_labels)]
pub unsafe fn return_to_user(context: &Armv8SyscallContext) -> ! {
    // set the entry point address
    ELR_EL1.set(context.elr);
    // set the stack pointer
    SP_EL0.set(context.sp);

    // configure the execution state for EL0:
    // - interrupts unmasked
    // - el0 exception level
    // - use sp_el0 stack pointer
    // - aarch64 execution state
    SPSR_EL1.write(
        SPSR_EL1::D::Masked + SPSR_EL1::A::Masked + SPSR_EL1::I::Unmasked
        + SPSR_EL1::F::Masked + SPSR_EL1::M::EL0t
    );

    // TODO: zero out/copy all registers
    core::arch::asm!(
        // copy argument to register x0
        "mov x0, {}",
        // return to address specified in elr_el1
        "eret",
        in(reg) context.x0,
        options(noreturn)
    )
}

/// Service a system call according to the ABI defined in [`twizzler_abi`]
pub fn handle_syscall(ctx: &mut ExceptionContext) {
    let mut context: Armv8SyscallContext = Default::default();
    context.x0 = ctx.x0;
    context.x1 = ctx.x1;
    context.x2 = ctx.x2;
    context.x3 = ctx.x3;
    context.x4 = ctx.x4;
    context.x5 = ctx.x5;
    context.x6 = ctx.x6;
    context.x7 = ctx.x7;
    context.sp = ctx.sp;
    context.elr = ctx.elr;

    crate::thread::enter_kernel();
    crate::interrupt::set(true);
    
    crate::syscall::syscall_entry(&mut context);
    
    crate::interrupt::set(false);
    crate::thread::exit_kernel();

    // copy over result values to exception return context
    // we use registers x6 and x7 for this purpose
    ctx.x6 = context.x6;
    ctx.x7 = context.x7;

    // returning from here will restore the calling context
    // and then call `eret` to jump back to user space
}
