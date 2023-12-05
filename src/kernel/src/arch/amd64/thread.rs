use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
};

use twizzler_abi::{
    arch::XSAVE_LEN,
    object::{MAX_SIZE, NULLPAGE_SIZE},
    upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags, UpcallInfo, UpcallTarget},
};

use crate::{
    arch::amd64::gdt::set_kernel_stack,
    memory::VirtAddr,
    processor::KERNEL_STACK_SIZE,
    thread::{current_thread_ref, Thread},
};

use super::{interrupt::IsrContext, syscall::X86SyscallContext};

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
    pub entry_registers: RefCell<Registers>,
    /// The frame of an upcall to restore. The restoration path only occurs on the first
    /// return-from-syscall after entering from the syscall that provides the frame to restore.
    /// We store that frame here until we hit the syscall return path, which then restores the
    /// frame and returns to user using this frame.
    pub upcall_restore_frame: RefCell<Option<UpcallFrame>>,
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
            entry_registers: RefCell::new(Registers::None),
            upcall_restore_frame: RefCell::new(None),
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

fn same_object(a: usize, b: usize) -> bool {
    a / MAX_SIZE == b / MAX_SIZE
}

pub fn set_upcall<T: UpcallAble + Copy>(
    regs: &mut T,
    target: UpcallTarget,
    info: UpcallInfo,
    sup: bool,
) -> bool
where
    UpcallFrame: From<T>,
{
    // Stack must always be 16-bytes aligned.
    const MIN_STACK_ALIGN: usize = 16;
    // We have to leave room for the red zone.
    const RED_ZONE_SIZE: usize = 512;
    // Frame must be aligned for the xsave region (Intel says aligned on 64 bytes).
    const MIN_FRAME_ALIGN: usize = 64;

    let current_stack_pointer = regs.get_stack_top();
    // We only switch contexts if it was requested and we aren't in that context.
    // TODO: once security contexts are more fully implemented, we'll need to change this code.
    let switch_to_super = sup && !same_object(target.super_stack, current_stack_pointer as usize);

    let target_addr = if switch_to_super {
        target.super_address
    } else {
        target.self_address
    };

    // If the address is not canonical, leave.
    let Ok(target_addr) = VirtAddr::new(target_addr as u64) else {
        return false;
    };

    let upcall_data = UpcallData {
        info,
        flags: if switch_to_super {
            UpcallHandlerFlags::SWITCHED_CONTEXT
        } else {
            UpcallHandlerFlags::empty()
        },
        source_ctx: 0.into(),
    };

    // Step 1: determine where we are going to put the frame. If we have
    // a supervisor stack, and we aren't currently on it, use that. Otherwise,
    // use the current stack pointer.
    let stack_pointer = if switch_to_super {
        target.super_stack as u64
    } else {
        current_stack_pointer
    };

    // TODO: once security contexts are more implemented, we'll need to do a bunch of permission checks
    // on the stack and target jump addresses.

    // Don't touch the red zone for the function we were in.
    let stack_top = stack_pointer - RED_ZONE_SIZE as u64;
    let stack_top = stack_top & (!(MIN_STACK_ALIGN as u64 - 1));

    // Step 2: compute all the sizes for things we're going to shuffle around, and check
    // if we even have enough space.
    let data_size = core::mem::size_of::<UpcallData>();
    let data_size = (data_size + MIN_STACK_ALIGN) & !(MIN_STACK_ALIGN - 1);
    let frame_size = core::mem::size_of::<UpcallFrame>();
    let frame_size = (frame_size + MIN_FRAME_ALIGN) & !(MIN_FRAME_ALIGN - 1);
    let data_start = stack_top - data_size as u64;

    // Frame needs extra care, since it must be aligned on 64-bytes for the xsave region.
    let frame_highest_start = data_start as usize - frame_size;
    let frame_padding = frame_highest_start - (frame_highest_start & !(MIN_FRAME_ALIGN - 1));
    let frame_start = data_start - (frame_size + frame_padding) as u64;
    assert_eq!(
        frame_start,
        frame_highest_start as u64 & !(MIN_FRAME_ALIGN as u64 - 1)
    );
    assert_eq!(frame_size & (MIN_FRAME_ALIGN - 1), 0);

    let total_size = data_size + frame_size + frame_padding + RED_ZONE_SIZE;
    let total_size = (total_size + MIN_STACK_ALIGN) & !(MIN_STACK_ALIGN - 1);

    let stack_object_base = (stack_top as usize / MAX_SIZE) * MAX_SIZE + NULLPAGE_SIZE;
    if stack_object_base + total_size >= stack_pointer as usize {
        // No space for our frame!
        return false;
    }

    // Step 3: write out the frame and the data into the stack.
    let data_ptr = data_start as usize as *mut UpcallData;
    let frame_ptr = frame_start as usize as *mut UpcallFrame;
    let mut frame: UpcallFrame = (*regs).into();

    // Step 3a: we need to fill out some extra stuff in the upcall frame, like the thread pointer and fpu state.
    frame.thread_ptr = current_thread_ref()
        .unwrap()
        .arch
        .user_fs
        .load(Ordering::SeqCst);

    unsafe {
        // We still need to save the fpu registers / sse state.
        if use_xsave() {
            core::arch::asm!("xsave [{}]", in(reg) frame.xsave_region.as_ptr(), in("rax") 3, in("rdx") 0);
        } else {
            core::arch::asm!("fxsave [{}]", in(reg) frame.xsave_region.as_ptr());
        }
        data_ptr.write(upcall_data);
        frame_ptr.write(frame);
    }

    // Step 4: final alignment, and then call into the context (either syscall or interrupt) code
    // to do the final setup of registers for the upcall.
    let stack_start = frame_start - MIN_STACK_ALIGN as u64;
    let stack_start = stack_start & !(MIN_STACK_ALIGN as u64 - 1);
    // We have to enter with a mis-aligned stack, so that the function prelude
    // of the receiver will re-align it. In this case, we control the ABI, so
    // we preserve this just for consistency.
    let stack_start = stack_start - core::mem::size_of::<u64>() as u64;

    regs.set_upcall(target_addr, frame_start, data_start, stack_start);
    true
}

pub(super) fn use_xsave() -> bool {
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
    pub fn restore_upcall_frame(&self, frame: &UpcallFrame) {
        // We restore this in the syscall return code path, since
        // we know that's where we are coming from, and we actually need
        // to use the ISR return mechanism (see the syscall code).
        *self.arch.upcall_restore_frame.borrow_mut() = Some(*frame);
    }

    pub fn arch_queue_upcall(&self, target: UpcallTarget, info: UpcallInfo, sup: bool) {
        match *self.arch.entry_registers.borrow() {
            Registers::None => {
                panic!("tried to upcall to a thread that hasn't started yet");
            }
            Registers::Interrupt(int, _) => {
                let int = unsafe { &mut *int };
                set_upcall(int, target, info, sup);
            }
            Registers::Syscall(sys, _) => {
                let sys = unsafe { &mut *sys };
                set_upcall(sys, target, info, sup);
            }
        }
    }

    pub fn set_entry_registers(&self, regs: Registers) {
        (*self.arch.entry_registers.borrow_mut()) = regs;
    }

    pub fn set_tls(&self, tls: u64) {
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
