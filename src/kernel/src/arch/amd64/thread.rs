use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use x86_64::VirtAddr;

use crate::{
    arch::amd64::desctables::set_kernel_stack, processor::KERNEL_STACK_SIZE, thread::Thread,
};

const XSAVE_LEN: usize = 1024;

#[repr(align(64))]
struct AlignedXsaveRegion([u8; XSAVE_LEN]);
pub struct ArchThread {
    xsave_region: AlignedXsaveRegion,
    rsp: core::cell::UnsafeCell<u64>,
    pub user_fs: AtomicU64,
    xsave_inited: AtomicBool,
    //user_gs: u64,
}
unsafe impl Sync for ArchThread {}

#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
unsafe extern "C" fn __do_switch(
    newsp: *const u64,       //rdi
    oldsp: *mut u64,         //rsi
    newlock: *mut AtomicU64, //rdx
    oldlock: *mut AtomicU64, //rcx
) {
    asm!(
        /* save registers */
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "pushfq",
        /* save the stack pointer. */
        "mov [rsi], rsp",
        /* okay, now we can release the switch lock */
        "mov qword ptr [rcx], 0",
        "sfence",
        /* try to grab the new switch lock for the new thread. if we fail, jump to a spin loop */
        "mov rax, [rdx]",
        "test rax, rax",
        "jnz sw_wait",
        "do_the_switch:",
        /* we can just store to the new switch lock, since we're guaranteed to be the only CPU here */
        "mov qword ptr [rdx], 1",
        /* okay, now load the new stack pointer and restore */
        "mov rsp, [rdi]",
        "popfq",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        /* finally, get the return address pushed by the caller of this function, and jump */
        "pop rax",
        "jmp rax",
        "sw_wait:",
        /* okay, so we have to wait. Just keep retrying to read zero from the lock, pausing in the meantime */
        "pause",
        "mov rax, [rdx]",
        "test rax, rax",
        "jnz sw_wait",
        "jmp do_the_switch",
        options(noreturn),
    )
}

impl ArchThread {
    pub fn new() -> Self {
        Self {
            xsave_region: AlignedXsaveRegion([0; XSAVE_LEN]),
            rsp: core::cell::UnsafeCell::new(0),
            user_fs: AtomicU64::new(0),
            xsave_inited: AtomicBool::new(false),
        }
    }
}

impl Default for ArchThread {
    fn default() -> Self {
        Self::new()
    }
}

impl Thread {
    pub fn set_tls(&self, tls: u64) {
        //logln!("setting user fs to {}", tls);
        self.arch.user_fs.store(tls, Ordering::SeqCst);
    }

    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        unsafe {
            set_kernel_stack(
                VirtAddr::new(self.kernel_stack.as_ref() as *const u8 as u64) + KERNEL_STACK_SIZE,
            );
            /*
            logln!(
                "switch {} -> {} {:?}",
                old_thread.id(),
                self.id(),
                self.arch.xsave_region.0.as_ptr()
            );
            */
            asm!("xsave [{}]", in(reg) old_thread.arch.xsave_region.0.as_ptr(), in("rax") 3, in("rdx") 0);
            /*
            for (i, x) in old_thread.arch.xsave_region.0.iter().enumerate() {
                if i % 16 == 0 {
                    log!("\n{}: ", i);
                }
                log!("{:x} ", x);
            }
            logln!("");
            */
            old_thread.arch.xsave_inited.store(true, Ordering::SeqCst);
            if self.arch.xsave_inited.load(Ordering::SeqCst) {
                asm!("xrstor [{}]", in(reg) self.arch.xsave_region.0.as_ptr(), in("rax") 3, in("rdx") 0);
            } else {
                let mut f: u16 = 0;
                let mut x: u32 = 0;
                asm!(
                    "finit",
                    "fstcw [rax]",
                    "or qword ptr [rax], 0x33f",
                    "fldcw [rax]",
                    "stmxcsr [rdx]",
                    "mfence",
                    "or qword ptr [rdx], 0x1f80",
                    "sfence",
                    "ldmxcsr [rdx]",
                    "stmxcsr [rdx]",
                    in("rax") &mut f, in("rdx") &mut x);
            }
        }
        let old_stack_save = old_thread.arch.rsp.get();
        let new_stack_save = self.arch.rsp.get();
        assert!(old_thread.switch_lock.load(Ordering::SeqCst) != 0);
        unsafe {
            __do_switch(
                new_stack_save,
                old_stack_save,
                core::intrinsics::transmute(&self.switch_lock),
                core::intrinsics::transmute(&old_thread.switch_lock),
            );
        }
    }

    pub unsafe fn init_va(&mut self, jmptarget: u64) {
        let stack = self.kernel_stack.as_ptr() as *mut u64;
        stack.add((KERNEL_STACK_SIZE / 8) - 2).write(jmptarget);
        stack.add((KERNEL_STACK_SIZE / 8) - 3).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 4).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 5).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 6).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 7).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 8).write(0);
        stack.add((KERNEL_STACK_SIZE / 8) - 9).write(0x202); //initial rflags: int-enabled, and reserved bit
        self.arch.rsp = core::cell::UnsafeCell::new(stack.add((KERNEL_STACK_SIZE / 8) - 9) as u64);
    }

    pub unsafe fn init(&mut self, f: extern "C" fn()) {
        self.init_va(f as usize as u64);
    }
}
