use core::{
    cell::RefCell,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};

use twizzler_abi::{
    arch::{ArchRegisters, XSAVE_LEN},
    object::{MAX_SIZE, NULLPAGE_SIZE, ObjID},
    thread::ExecutionState,
    upcall::{
        UPCALL_EXIT_CODE, UpcallData, UpcallFrame, UpcallHandlerFlags, UpcallInfo, UpcallTarget,
    },
};
use twizzler_rt_abi::error::TwzError;

use super::{interrupt::IsrContext, syscall::X86SyscallContext};
use crate::{
    arch::amd64::gdt::set_kernel_stack,
    memory::VirtAddr,
    processor::KERNEL_STACK_SIZE,
    thread::{Thread, current_thread_ref},
};

#[derive(Debug, Clone, Copy)]
pub enum Registers {
    None,
    Syscall(*mut X86SyscallContext),
    Interrupt(*mut IsrContext),
}

#[derive(Debug)]
struct RegistersPtr {
    ptr: AtomicU64,
}

impl RegistersPtr {
    pub fn new() -> Self {
        Self {
            ptr: AtomicU64::new(0),
        }
    }

    pub unsafe fn read_syscall(&self) -> Option<*mut X86SyscallContext> {
        let ptr = self.ptr.load(Ordering::SeqCst);
        if ptr == 0 || ptr & 1 != 0 {
            return None;
        }
        Some(ptr as *mut X86SyscallContext)
    }

    pub unsafe fn read_interrupt(&self) -> Option<*mut IsrContext> {
        let ptr = self.ptr.load(Ordering::SeqCst);
        if ptr == 0 || ptr & 1 == 0 {
            return None;
        }
        Some((ptr & !1) as *mut IsrContext)
    }

    pub fn as_registers(&self) -> Registers {
        let ptr = self.ptr.load(Ordering::SeqCst);
        if ptr == 0 {
            return Registers::None;
        }
        if ptr & 1 == 0 {
            Registers::Syscall(ptr as *mut X86SyscallContext)
        } else {
            Registers::Interrupt((ptr & !1) as *mut IsrContext)
        }
    }

    pub fn set_syscall(&self, ctx: *mut X86SyscallContext) {
        self.ptr.store(ctx as u64, Ordering::SeqCst);
    }

    pub fn set_interrupt(&self, ctx: *mut IsrContext) {
        self.ptr.store((ctx as u64) | 1, Ordering::SeqCst);
    }
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
    entry_registers: RegistersPtr,
    /// The frame of an upcall to restore. The restoration path only occurs on the first
    /// return-from-syscall after entering from the syscall that provides the frame to restore.
    /// We store that frame here until we hit the syscall return path, which then restores the
    /// frame and returns to user using this frame.
    upcall_restore_frame: RefCell<Option<UpcallFrame>>,
    //user_gs: u64,
}
unsafe impl Sync for ArchThread {}
unsafe impl Send for ArchThread {}

impl ArchThread {
    pub fn take_upcall_restore_frame(&self) -> Option<UpcallFrame> {
        self.upcall_restore_frame.try_borrow_mut().ok()?.take()
    }

    pub fn has_upcall_restore_frame(&self) -> bool {
        self.upcall_restore_frame
            .try_borrow()
            .ok()
            .is_some_and(|x| x.is_some())
    }
}

#[allow(named_asm_labels)]
#[unsafe(no_mangle)]
#[unsafe(naked)]
unsafe extern "C" fn __do_switch(
    newsp: *const u64,       //rdi
    oldsp: *mut u64,         //rsi
    newlock: *mut AtomicU64, //rdx
    oldlock: *mut AtomicU64, //rcx
) {
    core::arch::naked_asm!(
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
        "sfence",
        /* okay, now we can release the switch lock. We can probably relax this, but for now do
         * a seq_cst store (mov + mfence).
         * This store, after the rsp save above, is what `Thread::has_left_kernel_stack` reads
         * to decide the outgoing thread's stack is dead. Moving it earlier hands that
         * stack to another thread while this one is still on it. */
        "mov qword ptr [rcx], 0",
        "mfence",
        /* try to grab the new switch lock for the new thread. if we fail, jump to a spin loop.
         * We use lock xchg to ensure single winner for setting the lock, which has seq_cst
         * semantics. */
        "grab_the_lock:",
        "mov rax, 1",
        "lock xchg rax, [rdx]",
        "test rax, rax",
        "jnz sw_wait",
        "do_the_switch:",
        "mfence",
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
        /* okay, so we have to wait. Just keep retrying to read zero from the lock, pausing in
         * the meantime */
        "pause",
        "mov rax, [rdx]",
        "test rax, rax",
        "jnz sw_wait",
        "jmp grab_the_lock",
    )
}

impl ArchThread {
    pub fn new() -> Self {
        Self {
            xsave_region: AlignedXsaveRegion([0; XSAVE_LEN]),
            rsp: core::cell::UnsafeCell::new(0),
            user_fs: AtomicU64::new(0),
            xsave_inited: AtomicBool::new(false),
            entry_registers: RegistersPtr::new(),
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
    fn get_base_pointer(&self) -> u64 {
        0
    }
}

fn set_upcall<T: UpcallAble + Copy>(
    regs: &mut T,
    target: UpcallTarget,
    info: UpcallInfo,
    source_ctx: ObjID,
    thread_id: ObjID,
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
    // Minimum amount of stack space we need left over for execution
    const MIN_STACK_REMAINING: usize = 1024 * 1024; // 1MB

    let current_stack_pointer = regs.get_stack_top();
    // We only switch contexts if it was requested and we aren't in that context.
    // TODO: once security contexts are more fully implemented, we'll need to change this code.
    let switch_to_super = sup
        && !(current_stack_pointer as usize >= target.super_stack
            && (current_stack_pointer as usize) < (target.super_stack + target.super_stack_size));

    let target_addr = if switch_to_super {
        target.super_address
    } else {
        target.self_address
    };

    if target_addr == 0 {
        logln!("warning -- upcall to target address 0");
        return false;
    }

    // If the address is not canonical, leave.
    let Ok(target_addr) = VirtAddr::new(target_addr as u64) else {
        logln!("warning -- thread aborted to non-canonical jump address for upcall");
        return false;
    };

    let upcall_data = UpcallData {
        info,
        flags: if switch_to_super {
            UpcallHandlerFlags::SWITCHED_CONTEXT
        } else {
            UpcallHandlerFlags::empty()
        },
        source_ctx,
        thread_id,
    };

    // Step 1: determine where we are going to put the frame. If we have
    // a supervisor stack, and we aren't currently on it, use that. Otherwise,
    // use the current stack pointer.
    let stack_pointer = if switch_to_super {
        (target.super_stack + target.super_stack_size) as u64
    } else {
        current_stack_pointer
    };

    if stack_pointer == 0 {
        logln!("warning -- thread aborted to null stack pointer for upcall");
        return false;
    }

    // TODO: once security contexts are more implemented, we'll need to do a bunch of permission
    // checks on the stack and target jump addresses.

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

    if switch_to_super {
        if target.super_stack_size < (total_size + MIN_STACK_REMAINING) {
            logln!("warning -- thread aborted due to insufficient super stack space");
            return false;
        }
    } else {
        let stack_object_base = (stack_top as usize / MAX_SIZE) * MAX_SIZE + NULLPAGE_SIZE;
        if stack_object_base + (total_size + MIN_STACK_REMAINING) >= stack_pointer as usize {
            logln!("warning -- thread aborted due to insufficient stack space");
            return false;
        }
    }

    // Step 3: write out the frame and the data into the stack.
    let data_ptr = data_start as usize as *mut UpcallData;
    let frame_ptr = frame_start as usize as *mut UpcallFrame;
    let mut frame: UpcallFrame = (*regs).into();
    frame.prior_ctx = upcall_data.source_ctx;
    log::debug!("upcall frame: {:?}", frame);

    // Step 3a: we need to fill out some extra stuff in the upcall frame, like the thread pointer
    // and fpu state.
    frame.thread_ptr = current_thread_ref()
        .unwrap()
        .arch
        .user_fs
        .load(Ordering::SeqCst);

    unsafe {
        let (lower, upper) = xsave_mask();
        // We still need to save the fpu registers / sse state.
        if use_xsave() {
            if use_xsaveopt() {
                core::arch::asm!("xsaveopt [{}]", in(reg) frame.xsave_region.as_ptr(), in("rax") lower, in("rdx") upper);
            } else {
                core::arch::asm!("xsave [{}]", in(reg) frame.xsave_region.as_ptr(), in("rax") lower, in("rdx") upper);
            }
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

/// The `xsave`/`xrstor` state-component bitmap (EDX:EAX), as enabled in XCR0 by
/// `processor::init`.
///
/// Both instructions must use the *same* mask. They did not: the save used XCR0 while the restore
/// hardcoded 7 (x87 | SSE | AVX), so every component above bit 2 was saved and then never
/// restored, leaking between threads. That was live in both configurations -- the kernel enables
/// the MPX bits when the cpu reports them (qemu's TCG `-cpu max` does) and the AVX-512 bits on
/// hardware that has them -- and latent only because current userspace uses neither. Deriving the
/// mask in one place is the point: this drifted apart once already.
pub(super) fn xsave_mask() -> (u64, u64) {
    // Cached: XCR0 is set once per cpu in `processor::init` and never changed afterwards, but this
    // is read on every `xsave` *and* every `xrstor`, i.e. twice per context switch, and `xgetbv`
    // is not free. Safe to read whenever CR4.OSXSAVE is set, which `processor::init` does on every
    // cpu before any thread runs, and callers are all gated on `use_xsave()`.
    static XCR0: AtomicU64 = AtomicU64::new(0);
    let mut bits = XCR0.load(Ordering::Relaxed);
    if bits == 0 {
        bits = unsafe { x86::controlregs::xcr0() }.bits() as u64;
        XCR0.store(bits, Ordering::Relaxed);
    }
    (bits & 0xFFFFFFFF, bits >> 32)
}

/// Whether `xsaveopt` is available.
///
/// Same format as `xsave`, so `xrstor` is unchanged, but it skips writing components that are in
/// their initial state or unmodified since the last `xrstor` from the same buffer -- which for most
/// threads is nearly all of a 3 KiB region, on every context switch. The "unmodified" half is keyed
/// on the buffer address, so saving into a *different* buffer (the upcall frame) correctly falls
/// back to writing the component out.
pub(super) fn use_xsaveopt() -> bool {
    /// A/B switch.
    const USE_XSAVEOPT_IF_AVAILABLE: bool = true;
    static USE_XSAVEOPT: AtomicU8 = AtomicU8::new(0);
    if !USE_XSAVEOPT_IF_AVAILABLE {
        return false;
    }
    match USE_XSAVEOPT.load(Ordering::Relaxed) {
        0 => {
            let has = x86::cpuid::CpuId::new()
                .get_extended_state_info()
                .map(|i| i.has_xsaveopt())
                .unwrap_or(false);
            USE_XSAVEOPT.store(if has { 2 } else { 1 }, Ordering::Relaxed);
            has
        }
        1 => false,
        _ => true,
    }
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
    /// Restore an upcall frame. We don't actually immediately restore it,
    /// instead, we save the frame for when we return from the next syscall.
    /// Since this function is to be called by a frame restore syscall, that
    /// means we are here because of a syscall, so we know that code path will
    /// be the one with which we return to user. Note also that any upcalls
    /// generated to this thread after calling this function but before returning
    /// to userspace will cause the thread to immediately abort.
    pub fn restore_upcall_frame(&self, frame: &UpcallFrame) {
        if frame.ip() == 0 {
            logln!("warning -- tried to restore thread to 0 IP");
            crate::thread::exit(UPCALL_EXIT_CODE);
        }
        // The frame's own `thread_ptr` is applied later, on the syscall return path, so it wins
        // over whatever this switch installs for `prior_ctx`.
        let (res, _) = self.switch_sctx(frame.prior_ctx);
        if matches!(res, crate::security::SwitchResult::NotAttached) {
            logln!(
                "warning -- tried to restore thread to non-attached security context {}",
                frame.prior_ctx
            );
            crate::thread::exit(UPCALL_EXIT_CODE);
        }
        // We restore this in the syscall return code path, since
        // we know that's where we are coming from, and we actually need
        // to use the ISR return mechanism (see the syscall code).
        self.arch.upcall_restore_frame.borrow_mut().replace(*frame);
    }

    /// Queue up an upcall on this thread. The sup argument denotes if this upcall
    /// is requesting a supervisor context switch. Once this is done, the thread's kernel
    /// entry frame will be setup to enter the upcall handler on return-to-userspace.
    pub fn arch_queue_upcall(&self, target: UpcallTarget, info: UpcallInfo, sup: bool) {
        if self.arch.upcall_restore_frame.borrow().is_some() {
            logln!("warning -- thread aborted due to upcall generation during frame restoration");
            crate::thread::exit(UPCALL_EXIT_CODE);
        }
        let source_ctx = self.active_sctx_id();
        let ok = match self.arch.entry_registers.as_registers() {
            Registers::None => {
                panic!(
                    "tried to upcall {:?} to a thread that hasn't started yet",
                    info
                );
            }
            Registers::Interrupt(int) => {
                let int = unsafe { &mut *int };
                set_upcall(int, target, info, source_ctx, self.objid(), sup)
            }
            Registers::Syscall(sys) => {
                let sys = unsafe { &mut *sys };
                set_upcall(sys, target, info, source_ctx, self.objid(), sup)
            }
        };
        if !ok {
            logln!(
                "while trying to generate upcall: {:?} from {:?}",
                info,
                self.arch.entry_registers.as_registers()
            );
            return;
        }

        if sup {
            // Switch first: the switch installs whatever thread pointer this thread last used in
            // `super_ctx`, and the upcall target's is the one that must end up in place.
            self.switch_sctx(target.super_ctx);
            self.arch
                .user_fs
                .store(target.super_thread_ptr as u64, Ordering::SeqCst);
        }
    }

    pub fn set_entry_registers(&self, regs: Registers) {
        match regs {
            Registers::None => {
                self.arch.entry_registers.ptr.store(0, Ordering::SeqCst);
            }
            Registers::Syscall(ctx) => {
                assert!(!ctx.is_null());
                self.arch.entry_registers.set_syscall(ctx);
            }
            Registers::Interrupt(ctx) => {
                assert!(!ctx.is_null());
                self.arch.entry_registers.set_interrupt(ctx);
            }
        }
    }

    pub fn set_tls(&self, tls: u64) {
        self.arch.user_fs.store(tls, Ordering::SeqCst);
    }

    pub fn get_tls(&self) -> u64 {
        self.arch.user_fs.load(Ordering::SeqCst)
    }

    fn save_extended_state(&self) {
        let do_xsave = use_xsave();
        unsafe {
            let (lower, upper) = xsave_mask();

            if do_xsave {
                if use_xsaveopt() {
                    core::arch::asm!("xsaveopt [{}]", in(reg) self.arch.xsave_region.0.as_ptr(), in("rax") lower, in("rdx") upper);
                } else {
                    core::arch::asm!("xsave [{}]", in(reg) self.arch.xsave_region.0.as_ptr(), in("rax") lower, in("rdx") upper);
                }
            } else {
                core::arch::asm!("fxsave [{}]", in(reg) self.arch.xsave_region.0.as_ptr());
            }
        }
        self.arch.xsave_inited.store(true, Ordering::SeqCst);
    }

    fn restore_extended_state(&self) {
        let do_xsave = use_xsave();
        unsafe {
            if self.arch.xsave_inited.load(Ordering::SeqCst) {
                if do_xsave {
                    let (lower, upper) = xsave_mask();
                    core::arch::asm!("xrstor [{}]", in(reg) self.arch.xsave_region.0.as_ptr(), in("rax") lower, in("rdx") upper);
                } else {
                    core::arch::asm!("fxrstor [{}]", in(reg) self.arch.xsave_region.0.as_ptr());
                }
            } else {
                super::processor::init_fpu_state();
            }
        }
    }

    pub fn with_saved_extended_state<R>(&self, f: impl FnOnce() -> R) -> R {
        self.do_critical(move |_| {
            self.save_extended_state();
            let ret = f();
            self.restore_extended_state();
            ret
        })
    }

    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        assert!(!crate::interrupt::get());
        unsafe {
            set_kernel_stack(
                VirtAddr::new(self.kernel_stack.as_ptr() as u64)
                    .unwrap()
                    .offset(KERNEL_STACK_SIZE)
                    .unwrap(),
            );
        }

        old_thread.save_extended_state();
        // A thread that has never run has nothing saved and will not return from `__do_switch`
        // to restore itself, so give it a fresh fpu here. It cannot be running elsewhere, so
        // there is nothing to race.
        if !self.arch.xsave_inited.load(Ordering::SeqCst) {
            unsafe { super::processor::init_fpu_state() };
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
        // Reached only when `old_thread` is resumed. `__do_switch` returns holding its
        // switch_lock, which is the first moment this thread's saved extended state is ours to
        // read: the cpu we took it from writes `xsave_region` and only then releases that lock.
        // Restoring the *incoming* thread's state before the switch instead -- which is what
        // this did -- races that cpu, because `do_schedule`'s REINSERT branch can queue a thread
        // onto another cpu while it is still running here. The general-purpose registers were
        // safe (`__do_switch` loads them under the lock); the extended state was not, so a
        // migrating thread resumed with correct addresses and the SIMD registers it had at its
        // *previous* deschedule. Measured at 8-32 occurrences per suite run before this changed.
        old_thread.restore_extended_state();
    }

    pub unsafe fn init_va(&mut self, jmptarget: u64) {
        let stack = self.kernel_stack.as_ptr() as *mut u64;
        assert!(jmptarget != 0);
        unsafe {
            stack.add((KERNEL_STACK_SIZE / 8) - 2).write(jmptarget);
            stack.add((KERNEL_STACK_SIZE / 8) - 3).write(0);
            stack.add((KERNEL_STACK_SIZE / 8) - 4).write(42);
            stack.add((KERNEL_STACK_SIZE / 8) - 5).write(0);
            stack.add((KERNEL_STACK_SIZE / 8) - 6).write(0);
            stack.add((KERNEL_STACK_SIZE / 8) - 7).write(0);
            stack.add((KERNEL_STACK_SIZE / 8) - 8).write(0);
            stack.add((KERNEL_STACK_SIZE / 8) - 9).write(0x202); //initial rflags: int-enabled, and reserved bit
            self.arch.rsp =
                core::cell::UnsafeCell::new(stack.add((KERNEL_STACK_SIZE / 8) - 9) as u64);
        }
    }

    pub unsafe fn init(&mut self, f: extern "C" fn()) {
        unsafe {
            self.init_va(f as usize as u64);
        }
    }

    /// The thread's instruction pointer, or 0 if it cannot be read right now.
    ///
    /// `as_ptr`, deliberately not `borrow`/`try_borrow`. The callers that matter read this off
    /// *another* thread -- `check_system_hang` walks every thread -- and `BorrowRef::new` bumps the
    /// borrow counter with a *non-atomic* read-modify-write. Racing the owner's `borrow_mut` in
    /// `set_upcall_restore_frame`, that stale increment lands after the writer's flag and leaves
    /// the counter reading "shared" while a mutable guard is live. The owner's guard drop then
    /// trips `debug_assert!(is_writing(..))` and halts the cpu; in release, where that assert
    /// is compiled out, the drop instead latches the counter at 2 and a later `borrow_mut`
    /// hard-panics. `try_borrow` only stops the *reader* panicking -- it does not make the race
    /// go away.
    ///
    /// `as_ptr` touches no bookkeeping, so a racing read costs a torn value, not the machine. Zero
    /// is what this already returns for a thread with no registers, so callers tolerate it.
    pub fn read_ip(&self) -> u64 {
        use crate::syscall::SyscallContext;
        // SAFETY: best-effort read of a possibly-running thread; see the note above on why this
        // must not go through the borrow counter.
        let Some(frame) = (unsafe { &*self.arch.upcall_restore_frame.as_ptr() }) else {
            return match self.arch.entry_registers.as_registers() {
                Registers::None => 0,
                Registers::Interrupt(int) => {
                    let int = unsafe { &mut *int };
                    (*int).get_ip()
                }
                Registers::Syscall(sys) => {
                    let sys = unsafe { &mut *sys };
                    (*sys).pc().raw()
                }
            };
        };
        frame.rip
    }

    /// The thread's base pointer, or 0 if it cannot be read right now; see [`Thread::read_ip`].
    pub fn read_bp(&self) -> u64 {
        // SAFETY: as in `read_ip` -- must not touch the borrow counter.
        let Some(frame) = (unsafe { &*self.arch.upcall_restore_frame.as_ptr() }) else {
            return match self.arch.entry_registers.as_registers() {
                Registers::None => 0,
                Registers::Interrupt(int) => {
                    let int = unsafe { &mut *int };
                    (*int).get_stack_top()
                }
                Registers::Syscall(sys) => {
                    let sys = unsafe { &mut *sys };
                    (*sys).get_base_pointer()
                }
            };
        };
        // Was `.rip`: every other arm of this function returns a base pointer.
        frame.rbp
    }

    pub fn read_registers(&self) -> Result<ArchRegisters, TwzError> {
        if self.get_state() != ExecutionState::Suspended
            && self.id() != current_thread_ref().unwrap().id()
        {
            return Err(TwzError::Generic(
                twizzler_rt_abi::error::GenericError::AccessDenied,
            ));
        }
        let frame = &self.arch.upcall_restore_frame.borrow();
        if frame.is_none() {
            let frame = match self.arch.entry_registers.as_registers() {
                Registers::None => {
                    unreachable!()
                }
                Registers::Interrupt(int) => {
                    let int = unsafe { &mut *int };
                    (*int).into()
                }
                Registers::Syscall(sys) => {
                    let sys = unsafe { &mut *sys };
                    (*sys).into()
                }
            };
            return Ok(ArchRegisters {
                frame,
                fs: 0,
                gs: 0,
                es: 0,
                ds: 0,
                ss: 0,
                cs: 0,
            });
        }
        Ok(ArchRegisters {
            frame: frame.unwrap(),
            fs: 0,
            gs: 0,
            es: 0,
            ds: 0,
            ss: 0,
            cs: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use twizzler_kernel_macros::kernel_test;

    use crate::thread::current_thread_ref;

    #[kernel_test]
    fn test_with_saved_extended() {
        let thread = current_thread_ref().unwrap();
        thread.with_saved_extended_state(|| { /* TODO: SIMD test */ })
    }
}
