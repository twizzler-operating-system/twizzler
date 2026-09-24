/// CPU context (register state) switching.
///
/// NOTE: According to section 6.1.1 of the 64-bit ARM
/// Procedure Call Standard (PCS), not all registers
/// need to be saved, only those needed for a subroutine call.
///
/// A full detailed explanation can be found in the
/// "Procedure Call Standard for the Arm® 64-bit Architecture (AArch64)":
///     https://github.com/ARM-software/abi-aa/releases/download/2023Q1/aapcs64.pdf
use core::{
    cell::RefCell,
    sync::atomic::{AtomicU64, Ordering},
};

use arm64::registers::TPIDR_EL0;
use registers::interfaces::{Readable, Writeable};
use twizzler_abi::{
    arch::ArchRegisters,
    thread::ExecutionState,
    upcall::{UPCALL_EXIT_CODE, UpcallFrame, UpcallInfo, UpcallTarget},
};
use twizzler_rt_abi::error::TwzError;

use super::{exception::ExceptionContext, interrupt::DAIFMaskBits, syscall::Armv8SyscallContext};
use crate::{memory::VirtAddr, processor::KERNEL_STACK_SIZE, thread::Thread};

/// Registers that need to be saved between context switches.
///
/// x19-x30 and sp per AAPCS64 6.1.1, plus the user-visible FP/SIMD state: the kernel is built
/// without NEON and never touches v0-v31, so a switch is the only place they change hands.
#[derive(Default)]
#[repr(C)]
struct RegisterContext {
    x19: u64,
    x20: u64,
    x21: u64,
    x22: u64,
    x23: u64,
    x24: u64,
    x25: u64,
    x26: u64,
    x27: u64,
    x28: u64,
    x29: u64,
    // x30 aka the link register
    lr: u64,
    sp: u64,
    // thread local storage for user space
    tpidr: u64,
    tpidrro: u64,
    // interrupt state
    daif: u64,
    fpcr: u64,
    fpsr: u64,
    v: [u128; 32],
}

const CTX_FPCR: usize = core::mem::offset_of!(RegisterContext, fpcr);
const CTX_V: usize = core::mem::offset_of!(RegisterContext, v);
const _: () = assert!(CTX_FPCR == 16 * 8 && CTX_V == 144 && CTX_V % 16 == 0);

// arch specific thread state
#[repr(align(64))]
pub struct ArchThread {
    /// The register context to be managed during a context switch.
    context: RegisterContext,
    /// The register block saved on entry to handle and exception or interrupt.
    entry_registers: RefCell<*mut ExceptionContext>,
    /// The frame of an upcall to restore. The restoration path only occurs on the first
    /// return-from-syscall after entering from the syscall that provides the frame to restore.
    /// We store that frame here until we hit the syscall return path, which then restores the
    /// frame and returns to user using this frame.
    pub upcall_restore_frame: RefCell<Option<UpcallFrame>>,
}

unsafe impl Sync for ArchThread {}
unsafe impl Send for ArchThread {}

impl ArchThread {
    pub fn new() -> Self {
        Self {
            context: RegisterContext::default(),
            entry_registers: RefCell::new(core::ptr::null_mut()),
            upcall_restore_frame: RefCell::new(None),
        }
    }

    pub fn has_upcall_restore_frame(&self) -> bool {
        self.upcall_restore_frame
            .try_borrow()
            .ok()
            .is_some_and(|x| x.is_some())
    }
}

impl Default for ArchThread {
    fn default() -> Self {
        Self::new()
    }
}

// The alignment of addresses use by the stack
const CHECKED_STACK_ALIGNMENT: usize = 16;

/// Compute the top of the stack.
///
/// # Safety
/// The range from [stack_base, stack_base+stack_size] must be valid addresses.
pub fn new_stack_top(stack_base: usize, stack_size: usize) -> VirtAddr {
    let stack_addr = (stack_base + stack_size) as u64;
    // the stack pointer for aarch64 must be aligned to 16 bytes
    // since the stack is downwards descending, we align the address
    // down to be within the bounds.
    let stack_from_args = VirtAddr::new(stack_addr).unwrap();
    if stack_from_args.is_aligned_to(CHECKED_STACK_ALIGNMENT) {
        stack_from_args
    } else {
        stack_from_args
            .align_down(CHECKED_STACK_ALIGNMENT as u64)
            .unwrap()
    }
}

impl Thread {
    pub fn restore_upcall_frame(&self, frame: &UpcallFrame) {
        let user = |a: u64| VirtAddr::new(a).is_ok_and(|v| !v.is_kernel());
        if !user(frame.pc) || !user(frame.sp) {
            logln!(
                "warning -- thread aborted: resume frame pc {:#x} sp {:#x} is not user memory",
                frame.pc,
                frame.sp
            );
            crate::thread::exit(UPCALL_EXIT_CODE);
        }
        let (res, _) = self.switch_sctx(frame.prior_ctx);
        if matches!(res, crate::security::SwitchResult::NotAttached) {
            logln!("warning -- tried to restore thread to non-attached security context");
            crate::thread::exit(UPCALL_EXIT_CODE);
        }
        // We restore this in the syscall return code path, since
        // we know that's where we are coming from.
        *self.arch.upcall_restore_frame.borrow_mut() = Some(*frame);
    }

    pub fn arch_queue_upcall(&self, target: UpcallTarget, info: UpcallInfo, sup: bool) {
        if self.arch.upcall_restore_frame.borrow().is_some() {
            logln!("warning -- thread aborted due to upcall generation during frame restoration");
            crate::thread::exit(UPCALL_EXIT_CODE);
        }

        // obtain the active security context
        let source_ctx = self.active_sctx_id();

        // obtain a reference to the upcall frame register block
        // and set the upcall state in the register block
        // Copied out: `setup_upcall` writes the frame to user memory, which can fault, and the
        // fault handler must not find this borrow held.
        let regs_ptr = *self.arch.entry_registers.borrow();
        if !regs_ptr.is_null() {
            let ok = {
                let regs = unsafe { &mut *regs_ptr };
                regs.setup_upcall(target, info, source_ctx, self.objid(), sup)
            };
            if !ok {
                logln!(
                    "while trying to generate upcall: {:?} from {:?}",
                    info,
                    self.arch.entry_registers.borrow()
                );
                crate::thread::exit(UPCALL_EXIT_CODE);
            }
            if sup {
                self.switch_sctx(target.super_ctx);
                self.set_tls(target.super_thread_ptr as u64);
            }
        } else {
            panic!(
                "tried to upcall {:?} to a thread that hasn't started yet",
                info
            );
        }
    }

    /// Whether `sp` has run below this thread's own stack. Stacks are adjacent with no guard
    /// page, so the next frame lands on a neighbour's. A thread running on some other stack (the
    /// boot cpu, idle) is told by its saved sp and skipped.
    pub fn kernel_stack_overflowed(&self, sp: u64) -> bool {
        let base = self.kernel_stack.as_ptr() as u64;
        if base == 0 {
            return false;
        }
        let size = KERNEL_STACK_SIZE as u64;
        let own = self.arch.context.sp;
        own >= base && own < base + size && sp < base && sp + size > base
    }

    pub fn set_entry_registers(&self, regs: Option<*mut ExceptionContext>) {
        match regs {
            Some(r) => (*self.arch.entry_registers.borrow_mut()) = r,
            None => (*self.arch.entry_registers.borrow_mut()) = core::ptr::null_mut(),
        }
    }

    pub fn set_tls(&self, tls: u64) {
        TPIDR_EL0.set(tls);
    }

    pub fn get_tls(&self) -> u64 {
        TPIDR_EL0.get()
    }

    /// Architechture specific CPU context switch.
    ///
    /// On 64-bit ARM systems, we only need to save a few registers
    /// then switch thread stacks before changing control flow.
    /// Architechture specific CPU context switch.
    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        // Same handshake as amd64's `__do_switch`: release the outgoing thread's `switch_lock`
        // only after its registers and sp are saved, then acquire the incoming thread's before
        // touching its saved state. `has_left_kernel_stack` and cross-cpu pickup depend on it.
        // A zero here means another cpu released this thread's lock while it was running here:
        // it was freed and reused, or switched out under a stale current-thread pointer.
        assert!(
            old_thread.switch_lock.load(Ordering::SeqCst) != 0,
            "outgoing thread {} (state {:?}, exiting {}) has switch_lock 0; next {} on cpu {}",
            old_thread.id(),
            old_thread.get_state(),
            old_thread.is_exiting(),
            self.id(),
            crate::processor::mp::current_processor().id,
        );
        // An interrupt after the release below would push its frame onto a stack the reaper may
        // already have handed to another thread.
        assert!(!crate::interrupt::get());
        unsafe {
            __do_switch(
                core::intrinsics::transmute(&self.arch.context),
                core::intrinsics::transmute(&old_thread.arch.context),
                &self.switch_lock,
                &old_thread.switch_lock,
            );
        }
    }

    pub unsafe fn init(&mut self, entry: extern "C" fn()) {
        let stack = new_stack_top(self.kernel_stack.as_ptr() as usize, KERNEL_STACK_SIZE);
        // set the stack pointer as the last thing context (x30 + 1)
        self.arch.context.sp = stack.into();
        // set the link register as the second to last entry (x30)
        self.arch.context.lr = entry as u64;
        // by default interrupts are enabled (unmask the I bit)
        // in other words set bits D,A, and F in DAIF[9:6]
        self.arch.context.daif = (DAIFMaskBits::IRQ.complement().bits() as u64) << 6;
    }

    /// Not captured on aarch64: the amd64 path records the in-kernel `IsrContext` in
    /// `common_handler_entry`, and this arch's exception path has no equivalent hook yet. Samples
    /// therefore report 0, which the reporting side already treats as "no kernel pc".
    pub fn read_kernel_ip(&self) -> u64 {
        0
    }

    /// The user register state a sampler or debugger sees: the frame about to be restored, else
    /// the registers saved at the last entry from EL0; `None` when there are neither. Read through
    /// `as_ptr`, as on amd64: callers read other threads, and `borrow` would race the owner's
    /// `borrow_mut` on the non-atomic borrow counter. A torn read costs a wrong sample.
    fn user_frame(&self) -> Option<UpcallFrame> {
        unsafe {
            if let Some(frame) = *self.arch.upcall_restore_frame.as_ptr() {
                return Some(frame);
            }
            let regs = *self.arch.entry_registers.as_ptr();
            (!regs.is_null()).then(|| (*regs).into())
        }
    }

    pub fn read_ip(&self) -> u64 {
        self.user_frame().map_or(0, |f| f.pc)
    }

    /// Frame pointer (x29) at sampling time. Mirrors amd64's `read_bp`; without it the sampling
    /// path does not compile for this arch.
    pub fn read_bp(&self) -> u64 {
        self.user_frame().map_or(0, |f| f.x29)
    }

    /// Stack pointer at sampling time. See `ThreadSamplingEvent::sp` for why a frameless leaf
    /// needs this rather than the frame pointer.
    pub fn read_di_cx(&self) -> (u64, u64) {
        self.user_frame().map_or((0, 0), |f| (f.x0, f.x2))
    }

    pub fn read_sp(&self) -> u64 {
        self.user_frame().map_or(0, |f| f.sp)
    }

    pub fn read_registers(&self) -> Result<ArchRegisters, TwzError> {
        if self.get_state() != ExecutionState::Suspended {
            return Err(TwzError::Generic(
                twizzler_rt_abi::error::GenericError::AccessDenied,
            ));
        }
        let frame = self.user_frame().ok_or(TwzError::Generic(
            twizzler_rt_abi::error::GenericError::AccessDenied,
        ))?;
        Ok(ArchRegisters { frame })
    }
}

/// Copies the live FP/SIMD registers into `frame` (upcall delivery). The kernel is soft-float,
/// so they still hold the interrupted user code's values.
pub(super) fn save_fp_state(frame: &mut UpcallFrame) {
    unsafe {
        core::arch::asm!(
        ".arch_extension fp",
        ".arch_extension simd",
        "stp q0, q1, [{v}, #32 * 0]",
        "stp q2, q3, [{v}, #32 * 1]",
        "stp q4, q5, [{v}, #32 * 2]",
        "stp q6, q7, [{v}, #32 * 3]",
        "stp q8, q9, [{v}, #32 * 4]",
        "stp q10, q11, [{v}, #32 * 5]",
        "stp q12, q13, [{v}, #32 * 6]",
        "stp q14, q15, [{v}, #32 * 7]",
        "stp q16, q17, [{v}, #32 * 8]",
        "stp q18, q19, [{v}, #32 * 9]",
        "stp q20, q21, [{v}, #32 * 10]",
        "stp q22, q23, [{v}, #32 * 11]",
        "stp q24, q25, [{v}, #32 * 12]",
        "stp q26, q27, [{v}, #32 * 13]",
        "stp q28, q29, [{v}, #32 * 14]",
        "stp q30, q31, [{v}, #32 * 15]",
        "mrs {c}, fpcr",
        "mrs {s}, fpsr",
        v = in(reg) frame.v.as_mut_ptr(),
        c = out(reg) frame.fpcr,
        s = out(reg) frame.fpsr,
        options(nostack, preserves_flags),
        );
    }
}

/// Loads the FP/SIMD registers from `frame` (resume from upcall); runs with interrupts off on the
/// return-to-user path, so nothing can switch them out again before `eret`.
pub(super) fn restore_fp_state(frame: &UpcallFrame) {
    unsafe {
        core::arch::asm!(
        ".arch_extension fp",
        ".arch_extension simd",
        "ldp q0, q1, [{v}, #32 * 0]",
        "ldp q2, q3, [{v}, #32 * 1]",
        "ldp q4, q5, [{v}, #32 * 2]",
        "ldp q6, q7, [{v}, #32 * 3]",
        "ldp q8, q9, [{v}, #32 * 4]",
        "ldp q10, q11, [{v}, #32 * 5]",
        "ldp q12, q13, [{v}, #32 * 6]",
        "ldp q14, q15, [{v}, #32 * 7]",
        "ldp q16, q17, [{v}, #32 * 8]",
        "ldp q18, q19, [{v}, #32 * 9]",
        "ldp q20, q21, [{v}, #32 * 10]",
        "ldp q22, q23, [{v}, #32 * 11]",
        "ldp q24, q25, [{v}, #32 * 12]",
        "ldp q26, q27, [{v}, #32 * 13]",
        "ldp q28, q29, [{v}, #32 * 14]",
        "ldp q30, q31, [{v}, #32 * 15]",
        "msr fpcr, {c}",
        "msr fpsr, {s}",
        v = in(reg) frame.v.as_ptr(),
        c = in(reg) frame.fpcr,
        s = in(reg) frame.fpsr,
        options(nostack, readonly, preserves_flags),
        );
    }
}

/// The switch (1) saves x19-x30, sp, the EL0 thread pointers, DAIF, v0-v31 and FPCR/FPSR into the
/// outgoing thread's context, (2) releases its `switch_lock` and acquires the incoming thread's,
/// (3) restores that context and (4) `ret`s to its saved x30. The FP/SIMD restore sits under the
/// incoming lock on purpose: the cpu that saved it releases that lock only after the stores. Naked,
/// like amd64's: the saved x30 is *this* function's return address and the resumed thread returns
/// through it without an epilogue, so the function must own no frame -- an `assert!` in here once
/// cost every resume 16 bytes of sp.
#[unsafe(naked)]
unsafe extern "C" fn __do_switch(
    next: *const u64,           // x0
    current: *mut u64,          // x1
    new_lock: *const AtomicU64, // x2
    old_lock: *const AtomicU64, // x3
) {
    core::arch::naked_asm!(
        // The kernel target is soft-float; the assembler needs opting in for the q-register moves.
        ".arch_extension fp",
        ".arch_extension simd",
        // (1) save current thread's registers
        "stp x19, x20, [x1, #16 * 0]",
        "stp x21, x22, [x1, #16 * 1]",
        "stp x23, x24, [x1, #16 * 2]",
        "stp x25, x26, [x1, #16 * 3]",
        "stp x27, x28, [x1, #16 * 4]",
        "stp x29, x30, [x1, #16 * 5]",
        "mov x12, sp",
        "mrs x13, tpidr_el0",
        "mrs x14, tpidrro_el0",
        "mrs x15, daif",
        "stp x12, x13, [x1, #16 * 6]",
        "stp x14, x15, [x1, #16 * 7]",
        "mrs x16, fpcr",
        "mrs x17, fpsr",
        "stp x16, x17, [x1, #{fpcr}]",
        "stp q0, q1, [x1, #{v} + 32 * 0]",
        "stp q2, q3, [x1, #{v} + 32 * 1]",
        "stp q4, q5, [x1, #{v} + 32 * 2]",
        "stp q6, q7, [x1, #{v} + 32 * 3]",
        "stp q8, q9, [x1, #{v} + 32 * 4]",
        "stp q10, q11, [x1, #{v} + 32 * 5]",
        "stp q12, q13, [x1, #{v} + 32 * 6]",
        "stp q14, q15, [x1, #{v} + 32 * 7]",
        "stp q16, q17, [x1, #{v} + 32 * 8]",
        "stp q18, q19, [x1, #{v} + 32 * 9]",
        "stp q20, q21, [x1, #{v} + 32 * 10]",
        "stp q22, q23, [x1, #{v} + 32 * 11]",
        "stp q24, q25, [x1, #{v} + 32 * 12]",
        "stp q26, q27, [x1, #{v} + 32 * 13]",
        "stp q28, q29, [x1, #{v} + 32 * 14]",
        "stp q30, q31, [x1, #{v} + 32 * 15]",
        // (2) release the old lock now that the saved sp is visible; acquire the new one
        "stlr xzr, [x3]",
        "mov x10, #1",
        "2:",
        "ldaxr x9, [x2]",
        "cbnz x9, 3f",
        "stxr w9, x10, [x2]",
        "cbnz w9, 2b",
        "b 4f",
        "3:",
        "yield",
        "b 2b",
        "4:",
        // (3) restore next thread's registers
        "ldp x16, x17, [x0, #{fpcr}]",
        "msr fpcr, x16",
        "msr fpsr, x17",
        "ldp q0, q1, [x0, #{v} + 32 * 0]",
        "ldp q2, q3, [x0, #{v} + 32 * 1]",
        "ldp q4, q5, [x0, #{v} + 32 * 2]",
        "ldp q6, q7, [x0, #{v} + 32 * 3]",
        "ldp q8, q9, [x0, #{v} + 32 * 4]",
        "ldp q10, q11, [x0, #{v} + 32 * 5]",
        "ldp q12, q13, [x0, #{v} + 32 * 6]",
        "ldp q14, q15, [x0, #{v} + 32 * 7]",
        "ldp q16, q17, [x0, #{v} + 32 * 8]",
        "ldp q18, q19, [x0, #{v} + 32 * 9]",
        "ldp q20, q21, [x0, #{v} + 32 * 10]",
        "ldp q22, q23, [x0, #{v} + 32 * 11]",
        "ldp q24, q25, [x0, #{v} + 32 * 12]",
        "ldp q26, q27, [x0, #{v} + 32 * 13]",
        "ldp q28, q29, [x0, #{v} + 32 * 14]",
        "ldp q30, q31, [x0, #{v} + 32 * 15]",
        "ldp x19, x20, [x0, #16 * 0]",
        "ldp x21, x22, [x0, #16 * 1]",
        "ldp x23, x24, [x0, #16 * 2]",
        "ldp x25, x26, [x0, #16 * 3]",
        "ldp x27, x28, [x0, #16 * 4]",
        "ldp x29, x30, [x0, #16 * 5]",
        "ldp x12, x13, [x0, #16 * 6]",
        "ldp x14, x15, [x0, #16 * 7]",
        "msr tpidr_el0, x13",
        "msr tpidrro_el0, x14",
        "mov sp, x12",
        "msr daif, x15",
        // (4) resume at the saved x30
        "ret",
        fpcr = const CTX_FPCR,
        v = const CTX_V,
    )
}
