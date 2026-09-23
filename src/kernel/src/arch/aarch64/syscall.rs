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

use super::exception::ExceptionContext;
use crate::{memory::VirtAddr, syscall::SyscallContext, thread::current_thread_ref};

/// The register state needed to transition between kernel and user.
///
/// According to the ARM PCS Section 6, arguments/return values are
/// passed in via registers x0-x7
#[derive(Default, Clone, Copy, Debug)]
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

    fn get_return_values<R1, R2>(&mut self) -> (R1, R2)
    where
        u64: Into<R1>,
        u64: Into<R2>,
    {
        (self.x6.into(), self.x7.into())
    }
}

#[allow(named_asm_labels)]
pub unsafe fn return_to_user(context: &Armv8SyscallContext) -> ! {
    // Masked until `eret`: an interrupt taken after ELR/SP_EL0/SPSR are written returns through
    // its own handler's `msr elr_el1`/`msr spsr_el1`, and the `eret` below then lands on a kernel
    // pc at EL0.
    crate::interrupt::set(false);
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
        SPSR_EL1::D::Masked
            + SPSR_EL1::A::Masked
            + SPSR_EL1::I::Unmasked
            + SPSR_EL1::F::Masked
            + SPSR_EL1::M::EL0t,
    );

    // Only x0 carries anything; the rest would leak kernel register state to the new thread.
    core::arch::asm!(
        "mov x0, {}",
        "mov x1, xzr",
        "mov x2, xzr",
        "mov x3, xzr",
        "mov x4, xzr",
        "mov x5, xzr",
        "mov x6, xzr",
        "mov x7, xzr",
        "mov x8, xzr",
        "mov x9, xzr",
        "mov x10, xzr",
        "mov x11, xzr",
        "mov x12, xzr",
        "mov x13, xzr",
        "mov x14, xzr",
        "mov x15, xzr",
        "mov x16, xzr",
        "mov x17, xzr",
        "mov x18, xzr",
        "mov x19, xzr",
        "mov x20, xzr",
        "mov x21, xzr",
        "mov x22, xzr",
        "mov x23, xzr",
        "mov x24, xzr",
        "mov x25, xzr",
        "mov x26, xzr",
        "mov x27, xzr",
        "mov x28, xzr",
        "mov x29, xzr",
        "mov x30, xzr",
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

    // Results go to x6/x7 in the exception context before `exit_kernel`: a mailbox upcall queued
    // there snapshots `ctx` as the resume frame, and a copy made afterwards lands in the handler's
    // entry registers instead, so the interrupted call resumed with its syscall number as code.
    ctx.x6 = context.x6;
    ctx.x7 = context.x7;

    crate::interrupt::set(false);
    crate::thread::exit_kernel();

    // check if we are restoring an upcall frame, and if so, do that.
    handle_upcall(ctx);

    // returning from here will restore the calling context
    // and then call `eret` to jump back to user space
}

fn handle_upcall(ctx: &mut ExceptionContext) {
    let cur_th = current_thread_ref().unwrap();

    // if we have an upcall restore frame saved, fix up the register state
    // before we return to user space.
    let mut rf = cur_th.arch.upcall_restore_frame.borrow_mut();
    if let Some(mut up_frame) = rf.take() {
        // we MUST manually drop this
        drop(rf);

        super::thread::restore_fp_state(&up_frame);

        // restore the TLS registers which may have changed
        unsafe {
            core::arch::asm!(
                 "msr tpidr_el0, x13",
                 "msr tpidrro_el0, x14",
                 in("x13") up_frame.tpidr,
                 in("x14") up_frame.tpidrro
            );
            // modify the exception context registers directly
            // using the upcall frame given to us
            ctx.restore_from_upcall(&up_frame);
        }
    }
    // from here we return using the normal syscall/exception return path
}
