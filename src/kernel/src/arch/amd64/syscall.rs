use core::sync::atomic::Ordering;

use twizzler_abi::{arch::XSAVE_LEN, upcall::UpcallFrame};

use crate::{
    arch::thread::use_xsave, memory::VirtAddr, syscall::SyscallContext, thread::current_thread_ref,
};

use super::{
    interrupt::{return_with_frame_to_user, IsrContext},
    thread::{Registers, UpcallAble},
};

#[derive(Default, Clone, Copy, Debug)]
#[repr(C)]
pub struct X86SyscallContext {
    rax: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rbx: u64,
    r8: u64,
    r9: u64,
    r10: u64,
    r11: u64,
    r12: u64,
    r13: u64,
    r14: u64,
    r15: u64,
    rbp: u64,
    rcx: u64,
    rsp: u64,
}

impl From<X86SyscallContext> for UpcallFrame {
    fn from(int: X86SyscallContext) -> Self {
        Self {
            rip: int.rcx,
            rflags: int.r11,
            rsp: int.rsp,
            rbp: int.rbp,
            rax: int.rax,
            rbx: int.rbx,
            rcx: int.rcx,
            rdx: int.rdx,
            rdi: int.rdi,
            rsi: int.rsi,
            r8: int.r8,
            r9: int.r9,
            r10: int.r10,
            r11: 0,
            r12: int.r12,
            r13: int.r13,
            r14: int.r14,
            r15: int.r15,
            // these get filled out later
            xsave_region: [0; XSAVE_LEN],
            thread_ptr: 0,
            prior_ctx: 0.into(),
        }
    }
}

impl UpcallAble for X86SyscallContext {
    fn set_upcall(&mut self, target: VirtAddr, frame: u64, info: u64, stack: u64) {
        self.rcx = target.into();
        self.rdi = frame;
        self.rsi = info;
        self.rsp = stack;
    }

    fn get_stack_top(&self) -> u64 {
        self.rsp
    }
}

impl SyscallContext for X86SyscallContext {
    fn create_jmp_context(target: VirtAddr, stack: VirtAddr, arg: u64) -> Self {
        Self {
            rsp: stack.into(),
            rcx: target.into(),
            rdi: arg,
            ..Default::default()
        }
    }

    fn num(&self) -> usize {
        self.rax as usize
    }

    fn arg0<T: From<u64>>(&self) -> T {
        T::from(self.rdi)
    }

    fn arg1<T: From<u64>>(&self) -> T {
        T::from(self.rsi)
    }

    fn arg2<T: From<u64>>(&self) -> T {
        T::from(self.rdx)
    }

    fn arg3<T: From<u64>>(&self) -> T {
        T::from(self.r10)
    }

    fn arg4<T: From<u64>>(&self) -> T {
        T::from(self.r9)
    }

    fn arg5<T: From<u64>>(&self) -> T {
        T::from(self.r8)
    }

    fn pc(&self) -> VirtAddr {
        // TODO: check if this allows userspace to cause a kernel panic
        VirtAddr::new(self.rcx).unwrap()
    }

    fn set_return_values<R1, R2>(&mut self, ret0: R1, ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>,
    {
        self.rax = u64::from(ret0);
        self.rdx = u64::from(ret1);
    }
}

#[allow(named_asm_labels)]
pub unsafe fn return_to_user(context: *const X86SyscallContext) -> ! {
    core::arch::asm!(
        "cli",
        "mov rax, [r11 + 0x00]",
        "mov rdi, [r11 + 0x08]",
        "mov rsi, [r11 + 0x10]",
        "mov rdx, [r11 + 0x18]",
        "mov rbx, [r11 + 0x20]",
        "mov r8, [r11 + 0x28]",
        "mov r9, [r11 + 0x30]",
        "mov r10, [r11 + 0x38]",
        /* skip r11 until the end, since it's our context pointer */
        "mov r12, [r11 + 0x48]",
        "mov r13, [r11 + 0x50]",
        "mov r14, [r11 + 0x58]",
        "mov r15, [r11 + 0x60]",
        "mov rbp, [r11 + 0x68]",
        "mov rcx, [r11 + 0x70]",
        "mov rsp, [r11 + 0x78]",
        "mov r11, [r11 + 0x40]",
        "bts r11, 9",
        "swapgs",
        "sysretq",
        in("r11") context, options(noreturn))
}

#[no_mangle]
unsafe extern "C" fn syscall_entry_c(context: *mut X86SyscallContext, kernel_fs: u64) -> ! {
    if kernel_fs == 0 {
        panic!(
            "tried to set kernel fs to 0 in syscall from {:?}",
            context.as_ref().unwrap(),
        );
    }
    x86::msr::wrmsr(x86::msr::IA32_FS_BASE, kernel_fs);

    if true {
        logln!(
            "syscall entry {:?} {} {:x}",
            current_thread_ref().map(|ct| ct.id()),
            (*context).rax,
            (*context).rcx
        );
    }
    let t = current_thread_ref().unwrap();
    t.set_entry_registers(Registers::Syscall(context, *context));

    crate::thread::enter_kernel();
    crate::interrupt::set(true);
    drop(t);

    crate::syscall::syscall_entry(context.as_mut().unwrap());
    crate::interrupt::set(false);
    crate::thread::exit_kernel();

    /* We need this scope to drop the current thread reference before we return to user */
    let user_fs = {
        let cur_th = current_thread_ref().unwrap();
        let user_fs = cur_th.arch.user_fs.load(Ordering::SeqCst);
        // Okay, now check if we are restoring an upcall frame, and if so, do that. Unfortunately,
        // we can't use the sysret/exit instruction for this, since it clobbers registers. Instead,
        // we'll use the ISR return path, which doesn't.
        let mut rf = cur_th.arch.upcall_restore_frame.borrow_mut();
        if let Some(up_frame) = rf.take() {
            // we MUST manually drop this, _and_ the current thread ref (a bit later), because otherwise we leave
            // them hanging when we trampoline back into userspace.
            drop(rf);

            // Restore the sse registers. These don't get restored by the isr return path, so we have to do it ourselves.
            if use_xsave() {
                core::arch::asm!("xrstor [{}]", in(reg) up_frame.xsave_region.as_ptr(), in("rax") 3, in("rdx") 0);
            } else {
                core::arch::asm!("fxrstor [{}]", in(reg) up_frame.xsave_region.as_ptr());
            }

            // Restore the thread pointer (it might have changed, and we also allow for it to change inside the upcall frame during the upcall)
            cur_th
                .arch
                .user_fs
                .store(up_frame.thread_ptr, Ordering::SeqCst);
            cur_th.set_entry_registers(Registers::None);
            drop(cur_th);

            let int_frame = IsrContext::from(up_frame);

            if int_frame.get_ip() < 0x1000 {
                logln!("UPRETURN FAIL: {:x}", int_frame.get_ip());
            }
            logln!(
                "{}: return from upret syscall",
                current_thread_ref().unwrap().id()
            );
            x86::msr::wrmsr(x86::msr::IA32_FS_BASE, up_frame.thread_ptr);
            return_with_frame_to_user(int_frame);
        }
        cur_th.set_entry_registers(Registers::None);
        user_fs
    };

    if (*context).rcx < 0x1000 {
        logln!("UPRETURN FAIL: {:x}", (*context).rcx);
    }
    logln!(
        "{:?}: return from syscall",
        current_thread_ref().map(|ct| ct.id())
    );
    x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    /* TODO: check that rcx is canonical */
    return_to_user(context);
}

#[allow(named_asm_labels)]
#[naked]
pub unsafe extern "C" fn syscall_entry() -> ! {
    core::arch::asm!(
        /* syscall can only come from userspace, so we can safely blindly swapgs */
        "swapgs",
        "mov gs:16, r11",     //backup r11, which contains rflags
        "mov r11, gs:0",      //load kernel stack pointer
        "mov [r11 - 8], rsp", //push user stack pointer
        "lea rsp, [r11 - 8]", //set stack pointer to correct place on kernel stack
        "mov r11, gs:16",     //restore r11
        /* save user registers. */
        "push rcx",
        "push rbp",
        "push r15",
        "push r14",
        "push r13",
        "push r12",
        "push r11",
        "push r10",
        "push r9",
        "push r8",
        "push rbx",
        "push rdx",
        "push rsi",
        "push rdi",
        "push rax",
        "mov rdi, rsp",
        "mov rsi, gs:8",
        "call syscall_entry_c",
        options(noreturn),
    )
}
