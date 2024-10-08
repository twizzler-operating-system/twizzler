/// CPU context (register state) switching.
///
/// NOTE: According to section 6.1.1 of the 64-bit ARM
/// Procedure Call Standard (PCS), not all registers
/// need to be saved, only those needed for a subroutine call.
///
/// A full detailed explanation can be found in the
/// "Procedure Call Standard for the Arm® 64-bit Architecture (AArch64)":
///     https://github.com/ARM-software/abi-aa/releases/download/2023Q1/aapcs64.pdf
use core::cell::RefCell;

use arm64::registers::TPIDR_EL0;
use registers::interfaces::Writeable;
use twizzler_abi::upcall::{UpcallFrame, UpcallInfo, UpcallTarget, UPCALL_EXIT_CODE};

use super::{exception::ExceptionContext, interrupt::DAIFMaskBits, syscall::Armv8SyscallContext};
use crate::{memory::VirtAddr, processor::KERNEL_STACK_SIZE, thread::Thread};

/// Registers that need to be saved between context switches.
///
/// According to section 6.1.1, we only need to preserve
/// registers x19-x30 and the stack pointer (sp).
#[derive(Default)]
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
}

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
        let res = self.secctx.switch_context(frame.prior_ctx);
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
        let source_ctx = self.secctx.active_id();

        // obtain a reference to the upcall frame register block
        // and set the upcall state in the register block
        if !self.arch.entry_registers.borrow().is_null() {
            let ok = {
                let regs = unsafe { &mut *(*self.arch.entry_registers.borrow()) };
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
        } else {
            panic!(
                "tried to upcall {:?} to a thread that hasn't started yet",
                info
            );
        }
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

    /// Architechture specific CPU context switch.
    ///
    /// On 64-bit ARM systems, we only need to save a few registers
    /// then switch thread stacks before changing control flow.
    #[inline(never)]
    pub extern "C" fn arch_switch_to(&self, old_thread: &Thread) {
        // The switch (1) saves registers x19-x30 and the stack pointer (sp)
        // onto the current thread's context save area (old_thread).
        // According to the 64-bit ARM PCS, this amount of context is fine.
        // Other registers are either caller saved, or pushed onto
        // the stack when taking an exception.
        // Then we (2) restore the registes from the next thread's (self) context
        // save area, (3) switch stacks, (4) and return control by returning
        // to the address in the link register (x30).
        unsafe {
            let current: *mut u64 = core::intrinsics::transmute(&old_thread.arch.context);
            let next: *const u64 = core::intrinsics::transmute(&self.arch.context);
            core::arch::asm!(
                // (1) save current thread's registers
                "stp x19, x20, [x11, #16 * 0]",
                "stp x21, x22, [x11, #16 * 1]",
                "stp x23, x24, [x11, #16 * 2]",
                "stp x25, x26, [x11, #16 * 3]",
                "stp x27, x28, [x11, #16 * 4]",
                // save the fp (x29) and the lr (x30)
                "stp x29, x30, [x11, #16 * 5]",
                // save stack pointer
                "mov x12, sp",
                // save the thread pointer registers
                "mrs x13, tpidr_el0",
                "mrs x14, tpidrro_el0",
                // save the current interrupt state
                "mrs x15, daif",
                "stp x12, x13, [x11, #16 * 6]",
                "stp x14, x15, [x11, #16 * 7]",
                // (2) restore next thread's regs
                "ldp x19, x20, [x10, #16 * 0]",
                "ldp x21, x22, [x10, #16 * 1]",
                "ldp x23, x24, [x10, #16 * 2]",
                "ldp x25, x26, [x10, #16 * 3]",
                "ldp x27, x28, [x10, #16 * 4]",
                // restore the fp (x29) and the lr (x30)
                "ldp x29, x30, [x10, #16 * 5]",
                // restore the thread pointer registers
                "ldp x12, x13, [x10, #16 * 6]",
                "ldp x14, x15, [x10, #16 * 7]",
                "msr tpidr_el0, x13",
                "msr tpidrro_el0, x14",
                // (3) switch thread stacks
                "mov sp, x12",
                // set the current interrupt state
                "msr daif, x15",
                // (4) execution resumes in the address
                // pointed to by the link register (x30)
                "ret",
                // assign inputs to temporary registers
                in("x11") current,
                in("x10") next,
            );
        }
    }

    // this does not need to be pub, might not needed for aarch64
    pub unsafe fn init_va(&mut self, _jmptarget: u64) {
        todo!()
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
}
