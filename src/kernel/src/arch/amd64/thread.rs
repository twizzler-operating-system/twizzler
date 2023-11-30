use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
};

use alloc::vec::Vec;
use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

use crate::{
    arch::amd64::gdt::set_kernel_stack, memory::VirtAddr, processor::KERNEL_STACK_SIZE,
    spinlock::Spinlock, thread::Thread,
};

use super::{interrupt::IsrContext, syscall::X86SyscallContext};

const XSAVE_LEN: usize = 1024;

#[derive(Copy, Clone, Debug)]
pub enum Registers {
    None,
    Syscall(*mut X86SyscallContext, X86SyscallContext),
    Interrupt(*mut IsrContext, IsrContext),
}

#[derive(Debug)]
struct Context {
    registers: Registers,
    xsave: AlignedXsaveRegion,
}

impl Context {
    pub fn new(registers: Registers) -> Self {
        Self {
            registers,
            // TODO: save
            xsave: AlignedXsaveRegion([0; XSAVE_LEN]),
        }
    }
}

#[derive(Debug)]
#[repr(align(64))]
struct AlignedXsaveRegion([u8; XSAVE_LEN]);
pub struct ArchThread {
    xsave_region: AlignedXsaveRegion,
    rsp: core::cell::UnsafeCell<u64>,
    pub user_fs: AtomicU64,
    xsave_inited: AtomicBool,
    upcall: Option<(usize, UpcallInfo)>,
    backup_context: Spinlock<Vec<Context>>,
    pub entry_registers: RefCell<Registers>,
    //user_gs: u64,
}
unsafe impl Sync for ArchThread {}
unsafe impl Send for ArchThread {}

#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
unsafe extern "C" fn __do_switch(
    newsp: *const u64,       //rdi
    oldsp: *mut u64,         //rsi
    newlock: *mut AtomicU64, //rdx
    oldlock: *mut AtomicU64, //rcx
) {
    core::arch::asm!(
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
            upcall: None,
            backup_context: Spinlock::new(Vec::new()),
            entry_registers: RefCell::new(Registers::None),
        }
    }
}

impl Default for ArchThread {
    fn default() -> Self {
        Self::new()
    }
}

pub trait UpcallAble {
    fn set_upcall(&mut self, target: VirtAddr, frame: u64, info: u64, stack: u64);
    fn get_stack_top(&self) -> u64;
}
pub fn set_upcall<T: UpcallAble + Copy>(regs: &mut T, target: VirtAddr, info: UpcallInfo)
where
    UpcallFrame: From<T>,
{
    let stack_top = regs.get_stack_top() - 512;
    let stack_top = stack_top & (!15);

    let info_size = core::mem::size_of::<UpcallInfo>();
    let info_size = (info_size + 16) & !15;
    let frame_size = core::mem::size_of::<UpcallFrame>();
    let frame_size = (frame_size + 16) & !15;
    let info_start = stack_top - info_size as u64;
    let frame_start = info_start - frame_size as u64;

    let info_ptr = info_start as usize as *mut UpcallInfo;
    let frame_ptr = frame_start as usize as *mut UpcallFrame;

    let frame = (*regs).into();

    unsafe {
        info_ptr.write(info);
        frame_ptr.write(frame);
    }
    let stack_start = frame_start - 16;
    let stack_start = stack_start & !15;
    let stack_start = stack_start - 8;

    regs.set_upcall(target, frame_start, info_start, stack_start);
}

fn use_xsave() -> bool {
    static USE_XSAVE: AtomicU8 = AtomicU8::new(0);
    let xs = USE_XSAVE.load(Ordering::SeqCst);
    match xs {
        0 => {
            let has_xsave = x86::cpuid::CpuId::new()
                .get_feature_info()
                .map(|f| f.has_xsave())
                .unwrap_or_default();
            USE_XSAVE.store(if has_xsave { 2 } else { 1 }, Ordering::SeqCst);
            has_xsave
        }
        1 => false,
        _ => true,
    }
}

/// Compute the top of the stack. 
/// 
/// # Safety
/// The range from [stack_base, stack_base+stack_size] must be valid addresses.
pub fn new_stack_top(stack_base: usize, stack_size: usize) -> VirtAddr {
    VirtAddr::new((stack_base + stack_size - 8) as u64).unwrap()
}

impl Thread {
    pub fn arch_queue_upcall(&self, target: VirtAddr, info: UpcallInfo) {
        self.arch
            .backup_context
            .lock()
            .push(Context::new(*self.arch.entry_registers.borrow()));
        match *self.arch.entry_registers.borrow() {
            Registers::None => {
                panic!("tried to upcall to a thread that hasn't started yet");
            }
            Registers::Interrupt(int, _) => {
                let int = unsafe { &mut *int };
                set_upcall(int, target, info);
            }
            Registers::Syscall(sys, _) => {
                let sys = unsafe { &mut *sys };
                set_upcall(sys, target, info);
            }
        }
    }

    pub fn set_entry_registers(&self, regs: Registers) {
        (*self.arch.entry_registers.borrow_mut()) = regs;
    }

    pub fn set_tls(&self, tls: u64) {
        //logln!("setting user fs to {}", tls);
        self.arch.user_fs.store(tls, Ordering::SeqCst);
    }

    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        unsafe {
            set_kernel_stack(
                VirtAddr::new(self.kernel_stack.as_ref() as *const u8 as u64)
                    .unwrap()
                    .offset(KERNEL_STACK_SIZE)
                    .unwrap(),
            );
            let do_xsave = use_xsave();
            if do_xsave {
                core::arch::asm!("xsave [{}]", in(reg) old_thread.arch.xsave_region.0.as_ptr(), in("rax") 3, in("rdx") 0);
            } else {
                core::arch::asm!("fxsave [{}]", in(reg) old_thread.arch.xsave_region.0.as_ptr());
            }
            old_thread.arch.xsave_inited.store(true, Ordering::SeqCst);
            if self.arch.xsave_inited.load(Ordering::SeqCst) {
                if do_xsave {
                    core::arch::asm!("xrstor [{}]", in(reg) self.arch.xsave_region.0.as_ptr(), in("rax") 3, in("rdx") 0);
                } else {
                    core::arch::asm!("fxrstor [{}]", in(reg) self.arch.xsave_region.0.as_ptr());
                }
            } else {
                let mut f: u16 = 0;
                let mut x: u32 = 0;
                core::arch::asm!(
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
