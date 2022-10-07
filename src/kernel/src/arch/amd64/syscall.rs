use core::sync::atomic::Ordering;

use twizzler_abi::upcall::UpcallFrame;
use x86_64::VirtAddr;

use crate::{syscall::SyscallContext, thread::current_thread_ref};

use super::thread::{Registers, UpcallAble};

#[derive(Default, Clone, Copy)]
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
        }
    }
}

impl UpcallAble for X86SyscallContext {
    fn set_upcall(&mut self, target: usize, frame: u64, info: u64, stack: u64) {
        self.rcx = target as u64;
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
            rsp: stack.as_u64(),
            rcx: target.as_u64(),
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
        VirtAddr::new(self.rcx)
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
    x86::msr::wrmsr(x86::msr::IA32_FS_BASE, kernel_fs);
    let t = current_thread_ref().unwrap();
    t.set_entry_registers(Registers::Syscall(context, *context));

    crate::thread::enter_kernel();
    crate::interrupt::set(true);
    if false {
        logln!(
            "syscall entry {} {} {:x}",
            current_thread_ref().unwrap().id(),
            (*context).rax,
            (*context).rcx
        );
    }
    crate::syscall::syscall_entry(context.as_mut().unwrap());
    crate::interrupt::set(false);
    crate::thread::exit_kernel();
    t.set_entry_registers(Registers::None);

    /* We need this scope to drop the current thread reference before we return to user */
    {
        let t = current_thread_ref().unwrap();
        let user_fs = t.arch.user_fs.load(Ordering::SeqCst);
        x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    }
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
