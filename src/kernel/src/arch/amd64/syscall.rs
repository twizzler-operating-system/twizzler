use x86_64::{instructions::segmentation::Segment64, VirtAddr};

use crate::thread::current_thread_ref;

#[derive(Default)]
#[repr(C)]
pub struct SyscallContext {
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

impl SyscallContext {
    pub fn create_jmp_context(target: VirtAddr, stack: VirtAddr, arg: u64) -> Self {
        Self {
            rsp: stack.as_u64(),
            rcx: target.as_u64(),
            rdi: arg,
            ..Default::default()
        }
    }

    pub unsafe fn num(&self) -> usize {
        self.rax as usize
    }
    pub unsafe fn arg0<T: From<u64>>(&self) -> T {
        T::from(self.rdi)
    }
    pub unsafe fn arg1<T: From<u64>>(&self) -> T {
        T::from(self.rsi)
    }
    pub unsafe fn arg2<T: From<u64>>(&self) -> T {
        T::from(self.rdx)
    }
    pub unsafe fn arg3<T: From<u64>>(&self) -> T {
        T::from(self.r10)
    }
    pub unsafe fn arg4<T: From<u64>>(&self) -> T {
        T::from(self.r9)
    }
    pub unsafe fn arg5<T: From<u64>>(&self) -> T {
        T::from(self.r8)
    }
    pub fn pc(&self) -> VirtAddr {
        VirtAddr::new(self.rcx)
    }

    pub fn set_return_values<R1, R2>(&mut self, ret0: R1, ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>,
    {
        self.rax = u64::from(ret0);
        self.rdx = u64::from(ret1);
    }
}

#[allow(named_asm_labels)]
pub unsafe fn return_to_user(context: *const SyscallContext) -> ! {
    asm!(
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
unsafe extern "C" fn syscall_entry_c(context: *mut SyscallContext, kernel_fs: u64) -> ! {
    /* TODO: avoid doing both of these? */
    x86_64::registers::segmentation::FS::write_base(VirtAddr::new(kernel_fs));
    x86::msr::wrmsr(x86::msr::IA32_FS_BASE, kernel_fs);

    crate::thread::enter_kernel();
    crate::interrupt::set(true);
    //logln!("got syscall {}", context.as_mut().unwrap().num());
    crate::interrupt::set(false);
    crate::thread::exit_kernel();

    let t = current_thread_ref().unwrap();
    let user_fs = t.arch.user_fs;
    x86_64::registers::segmentation::FS::write_base(VirtAddr::new(user_fs));
    x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    /* TODO: check that rcx is canonical */
    return_to_user(context);
}

#[allow(named_asm_labels)]
#[naked]
pub unsafe extern "C" fn syscall_entry() -> ! {
    asm!(
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
