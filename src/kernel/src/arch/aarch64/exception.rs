/// ARMv8 exception handling
///
/// Configuration of the exception vector table, and
/// Handling of synchronous (internal) exceptions.
/// External interrupts handled in [interrupts.rs]
///
/// We currently do not handle nested exceptions.
use core::fmt::{Display, Formatter, Result};

use arm64::registers::{ESR_EL1, TPIDRRO_EL0, TPIDR_EL0, VBAR_EL1};
use registers::{
    interfaces::{Readable, Writeable},
    registers::InMemoryRegister,
};
use twizzler_abi::{
    arch::syscall::SYSCALL_MAGIC,
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    upcall::{
        MemoryAccessKind, UpcallData, UpcallFrame, UpcallHandlerFlags, UpcallInfo, UpcallTarget,
        UPCALL_EXIT_CODE,
    },
};

use crate::{
    memory::{context::virtmem::PageFaultFlags, VirtAddr},
    thread::current_thread_ref,
};

core::arch::global_asm!(r#"
/// Exception Vector Table Definition for EL1 (Kernel)
.global __exception_vector_table

// Table must be aligned on a 2048 byte boundary (0x800)
.align {TABLE_ALIGNMENT}

// The vector table contains actual code for exception handlers.
// The table is organized into 4 sections, with 4 entries each.
// Each entry is 128 bytes, and thus aligned on such a boundary
// The entries are for handling Synchronous, IRQ, FIQ, or SError.
// The virtual address of the EVT is stored in the VBAR register.
//
// Currently we only handle exceptions while in the kernel (EL1)
__exception_vector_table:

// Handlers for exceptions using the current EL with SP_EL0 (user)
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}

// Taking an exception from the current EL with SP_EL1 (kernel)
// The exception is handled from EL1->EL1. The stack pointer from
// the kernel is preserved.
b sync_exception_handler_el1
.align {VECTOR_ALIGNMENT}
b interrupt_request_handler_el1
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}

// Handling an exception from a Lower EL that is running in AArch64.
// Lower meaning lower priviledge (EL0/user). Basically do we handle
// exceptions that occur in userspace (syscalls, etc.).
b sync_exception_handler_el0
.align {VECTOR_ALIGNMENT}
b interrupt_request_handler_el0
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}

// Handling of exceptions from a Lower EL that is running in AArch32
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
"#,
TABLE_ALIGNMENT = const 11, // 2^11 = 2048 = 0x800
VECTOR_ALIGNMENT = const 7, // 2^7 = 128 = 0x80
);

// TODO: check/set stack alignment for ExceptionContext

/// Registers that are save/resored when handling an exception
#[derive(Debug, Copy, Clone)]
pub struct ExceptionContext {
    pub x0: u64,
    pub x1: u64,
    pub x2: u64,
    pub x3: u64,
    pub x4: u64,
    pub x5: u64,
    pub x6: u64,
    pub x7: u64,
    pub x8: u64,
    pub x9: u64,
    pub x10: u64,
    pub x11: u64,
    pub x12: u64,
    pub x13: u64,
    pub x14: u64,
    pub x15: u64,
    pub x16: u64,
    pub x17: u64,
    pub x18: u64,
    pub x19: u64,
    pub x20: u64,
    pub x21: u64,
    pub x22: u64,
    pub x23: u64,
    pub x24: u64,
    pub x25: u64,
    pub x26: u64,
    pub x27: u64,
    pub x28: u64,
    pub x29: u64,
    pub x30: u64,
    /// The stack pointer, depending on the context where the exception
    /// occurred, this is either sp_el0 or sp_el1
    pub sp: u64,
    /// The program counter. The address where the exception occurred.
    pub elr: u64,
    /// The state of the processor (SPSR_EL1). Determines execution environment (e.g., interrupts)
    pub spsr: u64,
    /// The cause of a synchronous exception (ESR_EL1).
    pub esr: u64,
    /// The address where a the fault occured (FAR_EL1).
    pub far: u64,
}

impl ExceptionContext {
    // Save the register context onto the stack to be used by upcall handler
    // and modify the current register state to prepare a jump into the
    // upcall handler.
    pub(super) fn setup_upcall(
        &mut self,
        // pub fn set_upcall2<T: UpcallAble + Copy>(
        // regs: &mut T,
        target: UpcallTarget,
        info: UpcallInfo,
        source_ctx: ObjID,
        thread_id: ObjID,
        sup: bool,
    ) -> bool {
        // Stack must always be 16-bytes aligned.
        const MIN_STACK_ALIGN: usize = 16;
        // We have to leave room for the red zone.
        const RED_ZONE_SIZE: usize = 512;

        // Minimum amount of stack space we need left over for execution
        const MIN_STACK_REMAINING: usize = 1024 * 1024; // 1MB

        let current_stack_pointer = self.sp;
        // We only switch contexts if it was requested and we aren't in that context.
        // TODO: once security contexts are more fully implemented, we'll need to change this code.

        // TODO: verify that the stack ranges are correct here
        let switch_to_super = sup
            && !(current_stack_pointer as usize >= target.super_stack
                && (current_stack_pointer as usize)
                    < (target.super_stack + target.super_stack_size));

        let target_addr = if switch_to_super {
            target.super_address
        } else {
            target.self_address
        };

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
            // (target.super_stack + target.super_stack_size) as u64
            todo!("supervisor stack requested")
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
        let data_start = stack_top - data_size as u64;
        let frame_start = data_start - frame_size as u64;

        let total_size = data_size + frame_size + RED_ZONE_SIZE;
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
        // convert the calling context into an upcall frame
        let mut frame: UpcallFrame = (*self).into();

        // Step 3a: we need to fill out the TLS register state
        frame.tpidr = TPIDR_EL0.get();
        frame.tpidrro = TPIDRRO_EL0.get();

        // TODO: save fpu registers / sse state

        // write all register state and upcall information
        unsafe {
            data_ptr.write(upcall_data);
            frame_ptr.write(frame);
        }

        // Step 4: final alignment, and then call into the context code
        // to do the final setup of registers for the upcall.
        let stack_start = frame_start - MIN_STACK_ALIGN as u64;
        let stack_start = stack_start & !(MIN_STACK_ALIGN as u64 - 1);
        // We have to enter with a mis-aligned stack, so that the function prelude
        // of the receiver will re-align it. In this case, we control the ABI, so
        // we preserve this just for consistency.
        let stack_start = stack_start - core::mem::size_of::<u64>() as u64;

        // write down the arguments and things needed for the upcall
        // set the jump target
        self.elr = target_addr.raw();
        // set the stack pointer
        self.sp = stack_start;
        // set the upcall frame pointer as the first argument
        self.x0 = frame_start;
        // set the upcall info pointer as the second argument
        self.x1 = data_start;

        true
    }

    // Restore all register state from the upcall frame by overwriting our current registers
    pub(super) unsafe fn restore_from_upcall(&mut self, frame: &UpcallFrame) {
        self.x0 = frame.x0;
        self.x1 = frame.x1;
        self.x2 = frame.x2;
        self.x3 = frame.x3;
        self.x4 = frame.x4;
        self.x5 = frame.x5;
        self.x6 = frame.x6;
        self.x7 = frame.x7;
        self.x8 = frame.x8;
        self.x9 = frame.x9;
        self.x10 = frame.x10;
        self.x11 = frame.x11;
        self.x12 = frame.x12;
        self.x13 = frame.x13;
        self.x14 = frame.x14;
        self.x15 = frame.x15;
        self.x16 = frame.x16;
        self.x17 = frame.x17;
        self.x18 = frame.x18;
        self.x19 = frame.x19;
        self.x20 = frame.x20;
        self.x21 = frame.x21;
        self.x22 = frame.x22;
        self.x23 = frame.x23;
        self.x24 = frame.x24;
        self.x25 = frame.x25;
        self.x26 = frame.x26;
        self.x27 = frame.x27;
        self.x28 = frame.x28;
        self.x29 = frame.x29;
        self.x30 = frame.fp;
        self.sp = frame.sp;
        self.elr = frame.pc;
        self.spsr = frame.spsr;
    }
}

impl Display for ExceptionContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        writeln!(f, "ExceptionContext (registers x0-x30):")?;
        writeln!(
            f,
            "\tx0:  {:#018x} x1:  {:#018x} x2:  {:#018x} x3:  {:#018x}",
            self.x0, self.x1, self.x2, self.x3
        )?;
        writeln!(
            f,
            "\tx4:  {:#018x} x5:  {:#018x} x6:  {:#018x} x7:  {:#018x}",
            self.x4, self.x5, self.x6, self.x7
        )?;
        writeln!(
            f,
            "\tx8:  {:#018x} x9:  {:#018x} x10: {:#018x} x11: {:#018x}",
            self.x8, self.x9, self.x10, self.x11
        )?;
        writeln!(
            f,
            "\tx12: {:#018x} x13: {:#018x} x14: {:#018x} x15: {:#018x}",
            self.x12, self.x13, self.x14, self.x15
        )?;
        writeln!(
            f,
            "\tx16: {:#018x} x17: {:#018x} x18: {:#018x} x19: {:#018x}",
            self.x16, self.x17, self.x18, self.x19
        )?;
        writeln!(
            f,
            "\tx20: {:#018x} x21: {:#018x} x22: {:#018x} x23: {:#018x}",
            self.x20, self.x21, self.x22, self.x23
        )?;
        writeln!(
            f,
            "\tx24: {:#018x} x25: {:#018x} x26: {:#018x} x27: {:#018x}",
            self.x24, self.x25, self.x26, self.x27
        )?;
        writeln!(
            f,
            "\tx28: {:#018x} x29: {:#018x} x30: {:#018x}  sp: {:#018x}",
            self.x28, self.x29, self.x30, self.sp
        )?;
        writeln!(
            f,
            "\telr: {:#018x} spsr: {:#018x} esr: {:#018x} far: {:#018x}",
            self.elr, self.spsr, self.esr, self.far
        )
    }
}

impl From<ExceptionContext> for UpcallFrame {
    fn from(ctx: ExceptionContext) -> Self {
        let mut frame = UpcallFrame::default();

        frame.x0 = ctx.x0;
        frame.x1 = ctx.x1;
        frame.x2 = ctx.x2;
        frame.x3 = ctx.x3;
        frame.x4 = ctx.x4;
        frame.x5 = ctx.x5;
        frame.x6 = ctx.x6;
        frame.x7 = ctx.x7;
        frame.x8 = ctx.x8;
        frame.x9 = ctx.x9;
        frame.x10 = ctx.x10;
        frame.x11 = ctx.x11;
        frame.x12 = ctx.x12;
        frame.x13 = ctx.x13;
        frame.x14 = ctx.x14;
        frame.x15 = ctx.x15;
        frame.x16 = ctx.x16;
        frame.x17 = ctx.x17;
        frame.x18 = ctx.x18;
        frame.x19 = ctx.x19;
        frame.x20 = ctx.x20;
        frame.x21 = ctx.x21;
        frame.x22 = ctx.x22;
        frame.x23 = ctx.x23;
        frame.x24 = ctx.x24;
        frame.x25 = ctx.x25;
        frame.x26 = ctx.x26;
        frame.x27 = ctx.x27;
        frame.x28 = ctx.x28;
        frame.x29 = ctx.x29;
        frame.fp = ctx.x30;
        frame.sp = ctx.sp;
        frame.pc = ctx.elr;
        frame.spsr = ctx.spsr;

        frame
    }
}

/// macro creates a high level exception handler
/// to be used in the exception vector table.
/// saves/restores regs on the current stack pointer
/// and calls the specified handler
macro_rules! exception_handler {
    ($name:ident, $handler:ident, $is_kernel:tt) => {
        #[naked]
        #[no_mangle]
        pub(super) unsafe extern "C" fn $name() {
            core::arch::naked_asm!(
                // save all general purpose registers (x0-x30)
                // modify the stack pointer base
                "sub sp, sp, {FRAME_SIZE}",
                // push registers onto the stack
                "stp x0, x1, [sp]",
                "stp x2, x3, [sp, #16 * 1]",
                "stp x4, x5, [sp, #16 * 2]",
                "stp x6, x7, [sp, #16 * 3]",
                "stp x8, x9, [sp, #16 * 4]",
                "stp x10, x11, [sp, #16 * 5]",
                "stp x12, x13, [sp, #16 * 6]",
                "stp x14, x15, [sp, #16 * 7]",
                "stp x16, x17, [sp, #16 * 8]",
                "stp x18, x19, [sp, #16 * 9]",
                "stp x20, x21, [sp, #16 * 10]",
                "stp x22, x23, [sp, #16 * 11]",
                "stp x24, x25, [sp, #16 * 12]",
                "stp x26, x27, [sp, #16 * 13]",
                "stp x28, x29, [sp, #16 * 14]",
                // save other important registers
                // link register (i.e. x30)
                save_stack_pointer!($is_kernel),
                "stp x30, x10, [sp, #16 * 15]",
                // the program counter
                "mrs x11, elr_el1",
                // the processor state
                "mrs x12, spsr_el1",
                // the exception syndrome register
                "mrs x13, esr_el1",
                // the fault address register
                "mrs x14, far_el1",
                "stp x11, x12, [sp, #16 * 16]",
                "stp x13, x14, [sp, #16 * 17]",
                // move stack pointer of last frame as an argument
                "mov x0, sp",
                // go to exception handler (overwrites x30)
                "bl {handler}",
                // pop registers off of the stack
                "ldp x13, x14, [sp, #16 * 17]",
                "ldp x11, x12, [sp, #16 * 16]",
                // the program counter
                "msr elr_el1, x11",
                // the processor state
                "msr spsr_el1, x12",
                // the exception syndrome register
                "msr esr_el1, x13",
                // the fault address register
                "msr far_el1, x14",
                // restore all general purpose registers (x0-x30)
                "ldp x30, x10, [sp, #16 * 15]",
                restore_stack_pointer!($is_kernel),
                "ldp x28, x29, [sp, #16 * 14]",
                "ldp x26, x27, [sp, #16 * 13]",
                "ldp x24, x25, [sp, #16 * 12]",
                "ldp x22, x23, [sp, #16 * 11]",
                "ldp x20, x21, [sp, #16 * 10]",
                "ldp x18, x19, [sp, #16 * 9]",
                "ldp x16, x17, [sp, #16 * 8]",
                "ldp x14, x15, [sp, #16 * 7]",
                "ldp x12, x13, [sp, #16 * 6]",
                "ldp x10, x11, [sp, #16 * 5]",
                "ldp x8, x9, [sp, #16 * 4]",
                "ldp x6, x7, [sp, #16 * 3]",
                "ldp x4, x5, [sp, #16 * 2]",
                "ldp x2, x3, [sp, #16 * 1]",
                "ldp x0, x1, [sp]",
                // restore the stack pointer base
                "add sp, sp, {FRAME_SIZE}",
                // return from exception
                "eret",
                handler = sym $handler,
                FRAME_SIZE = const core::mem::size_of::<ExceptionContext>(),
            )
        }
    };
}

// copy the value of the stack pointer into x10
macro_rules! save_stack_pointer {
    // save kernel stack pointer
    (true) => {
        // restore stack pointer base
        "add x10, sp, {FRAME_SIZE}"
    };
    // save user stack pointer
    (false) => {
        // copy the value of sp_el0
        "mrs x10, sp_el0"
    };
}

// restore the value of the stack pointer from x10
macro_rules! restore_stack_pointer {
    // Restore the kernel stack pointer. This happens anyways since we clear
    // the stack frame used for the exception context and `sp` is aliased to
    // `sp_el1` in the kernel. So we return an empty string.
    (true) => {
        ""
    };
    // restore the user stack pointer (sp_el0)
    (false) => {
        // copy the value of sp_el0
        "msr sp_el0, x10"
    };
}

// export macro to be used, but only in the parent module
pub(super) use exception_handler;
pub(super) use restore_stack_pointer;
pub(super) use save_stack_pointer;

// Default exception handler simply prints out
// verbose debug information to the kernel console.
exception_handler!(default_exception_handler, debug_handler, true);

/// Exception handler prints information about the
/// stack frame that generated the exception and other
/// useful system register state. Then it panics.
fn debug_handler(ctx: &mut ExceptionContext) {
    // read of raw value for ESR
    let esr = ctx.esr;
    // print reason for exception (syndrome register)
    emerglogln!(
        "[kernel::exception] Exception Syndrome Register (ESR) value: {:#x}",
        esr
    );
    // print decoding information
    emerglogln!("[kernel::exception] decoding ESR");
    // ec: exception class
    let esr_reg: InMemoryRegister<u64, ESR_EL1::Register> = InMemoryRegister::new(esr);
    emerglogln!(
        "ESR[31:26] = {:#x} ==> EC (Exception Class)",
        esr_reg.read(ESR_EL1::EC)
    );
    let mut data_abort = false;
    emerglogln!(
        "\t{}",
        match esr_reg.read_as_enum(ESR_EL1::EC) {
            Some(ESR_EL1::EC::Value::SVC64) => "SVC instruction execution in AArch64 state.",
            Some(ESR_EL1::EC::Value::DataAbortCurrentEL) => {
                data_abort = true;
                "Data Abort taken without a change in Exception level."
            }
            Some(ESR_EL1::EC::Value::DataAbortLowerEL) => {
                data_abort = true;
                "Data Abort taken from a lower Exception level."
            }
            Some(ESR_EL1::EC::Value::InstrAbortLowerEL) =>
                "Instruction abort from a lower Exception level.",
            Some(ESR_EL1::EC::Value::Unknown) | _ => "Unknown reason.",
        }
    );
    // iss: syndrome
    let iss = esr_reg.read(ESR_EL1::ISS);
    emerglogln!(
        "ESR[24:0] = {:#x} ==> ISS (Instruction Specific Syndrome)",
        iss
    );

    // if a page fault occured, then decode the ISS accordingly
    if data_abort {
        // is the syndrome information in ISS[23:14] valid?
        let isv = iss & (1 << 24) != 0;
        emerglogln!(
            "\tISS[24] = {:#x} ==> ISV (Instruction Syndrome Valid)",
            (iss >> 24) & 0x1
        );
        emerglogln!(
            "\t\tSyndrome information in ISS[23:14] is{}valid",
            if isv { " " } else { " not " }
        );

        // is the fault address register valid?
        let far_valid = iss & (1 << 10) == 0;
        emerglogln!(
            "\tISS[10] = {:#x} ==> FnV (FAR not Valid)",
            (iss >> 10 & 0x1)
        );
        if far_valid {
            emerglogln!("\t\tFault Address Register is valid");
            // print faulting address (ELR/FAR)
            emerglogln!("\t\tFAR value = {:#018x}", ctx.far);
        }

        // was fault caused by a write to memory or a read?
        let write_fault = iss & (1 << 6) != 0;
        emerglogln!(
            "\tISS[6] = {:#x} ==> WnR (Write not Read)",
            (iss >> 6 & 0x1)
        );
        emerglogln!(
            "\t\tAbort caused by a memory {}",
            if write_fault { "write" } else { "read" }
        );

        // DFSC bits[5:0] indicate the type of fault
        let dfsc = iss & 0b111111;
        emerglogln!("\tISS[5:0] = {:#x} ==> DFSC (Data Fault Status Code)", dfsc);
        if dfsc & 0b111100 == 0b001000 {
            // we have an access fault
            let level = dfsc & 0b11;
            emerglogln!("\t\tAccess flag fault, level {}", level);
            // TODO: set the access flag
        }
    }

    // print other system registers: PSTATE/SPSR
    emerglogln!("[kernel::exception] SPSR_EL1: {:#018x}", ctx.spsr);

    // print registers
    emerglog!("[kernel::exception] dumping register state: {}", ctx);

    panic!("caught unhandled exception!!!")
}

// Exception handler that services synchronous exceptions
// the same base handler, `sync_handler` is used (for now)
// for both user and kernel.
exception_handler!(sync_exception_handler_el1, sync_handler, true);
exception_handler!(sync_exception_handler_el0, sync_handler, false);

/// Exception handler deals with synchronous exceptions
/// such as Data Aborts (i.e. page faults)
fn sync_handler(ctx: &mut ExceptionContext) {
    // read of raw value for ESR
    let esr = ctx.esr;
    let esr_reg: InMemoryRegister<u64, ESR_EL1::Register> = InMemoryRegister::new(esr);

    {
        let current_thread = current_thread_ref().unwrap();
        current_thread.set_entry_registers(Some(ctx as *mut ExceptionContext));
    }

    match esr_reg.read_as_enum(ESR_EL1::EC) {
        // TODO: reorganize data abort handling between user and kernel
        Some(ESR_EL1::EC::Value::DataAbortCurrentEL)
        | Some(ESR_EL1::EC::Value::DataAbortLowerEL) => {
            // iss: syndrome
            let iss = esr_reg.read(ESR_EL1::ISS);
            // is the fault address register valid?
            let far_valid = iss & (1 << 10) == 0;
            // print faulting address (ELR/FAR)
            let far = ctx.far;
            if !far_valid {
                panic!("FAR is not valid!!");
            }

            // was fault caused by a write to memory or a read?
            let write_fault = iss & (1 << 6) != 0;
            let cause = if write_fault {
                MemoryAccessKind::Write
            } else {
                MemoryAccessKind::Read
            };

            // TODO: support for PRESENT and INVALID flags
            let flags = PageFaultFlags::empty();

            let far_va = match VirtAddr::new(far as u64) {
                Ok(v) => v,
                Err(_) => panic!("non canonical address: {:x}", far),
            };

            // DFSC bits[5:0] indicate the type of fault
            let dfsc = iss & 0b111111;
            if dfsc & 0b111100 == 0b001000 {
                // we have an access fault
                let level = dfsc & 0b11;
                todo!("Access flag fault, level {}", level);
                // TODO: set the access flag
            } else if dfsc & 0b001100 == 0b001100 {
                let level = dfsc & 0b11;
                todo!("Permission fault, level {} {:?} {:?}", level, cause, far_va);
            }
            crate::thread::enter_kernel();
            crate::interrupt::set(true);
            let elr = ctx.elr;
            if let Ok(elr_va) = VirtAddr::new(elr) {
                crate::memory::context::virtmem::page_fault(far_va, cause, flags, elr_va);
            } else {
                todo!("send upcall exception info");
            }
            crate::interrupt::set(false);
            crate::thread::exit_kernel();
        }
        Some(ESR_EL1::EC::Value::InstrAbortLowerEL) => {
            handle_inst_abort(ctx, &esr_reg);
        }
        Some(ESR_EL1::EC::Value::SVC64) => {
            // iss: syndrome, contains passed to SVC
            let iss = esr_reg.read(ESR_EL1::ISS);
            if iss != SYSCALL_MAGIC {
                // TODO: handle this
                panic!("invalid syscall invocation");
            }
            super::syscall::handle_syscall(ctx);
        }
        Some(ESR_EL1::EC::Value::Unknown) | _ => debug_handler(ctx),
    }

    {
        let current_thread = current_thread_ref().unwrap();
        current_thread.set_entry_registers(None);
    }

    crate::interrupt::post_interrupt();
}

fn handle_inst_abort(
    ctx: &mut ExceptionContext,
    esr_reg: &InMemoryRegister<u64, ESR_EL1::Register>,
) {
    // decoding ISS for instruction fault.
    // iss: syndrome
    let iss = esr_reg.read(ESR_EL1::ISS);
    // is the fault address register valid? ... use bit 10
    let far_valid = iss & (1 << 10) == 0;
    if !far_valid {
        panic!("FAR is not valid!!");
    }
    let far = ctx.far;

    // The cause is from an instruction fetch
    let cause = MemoryAccessKind::InstructionFetch;

    // TODO: support for PRESENT and INVALID flags

    // NOTE: currently, only instruciton aborts are handled when coming from
    // user space, so we know that the page fault is user
    let flags = PageFaultFlags::USER;

    let far_va = VirtAddr::new(far as u64).unwrap();

    // IFSC bits[5:0] indicate the type of fault
    let ifsc = iss & 0b111111;
    if ifsc & 0b111100 == 0b001000 {
        // we have an access fault
        let level = ifsc & 0b11;
        todo!("Access flag fault, level {}", level);
        // TODO: set the access flag
    } else if ifsc & 0b001100 == 0b001100 {
        let level = ifsc & 0b11;
        todo!("Permission fault, level {}", level);
    } else if ifsc & 0b0000100 == 0b0000100 {
        // translation fault
        let _level = ifsc & 0b11;
    }

    crate::thread::enter_kernel();
    crate::interrupt::set(true);
    let elr = ctx.elr;
    if let Ok(elr_va) = VirtAddr::new(elr) {
        // logln!("fault {:?} from {:?}", far_va, elr_va);
        crate::memory::context::virtmem::page_fault(far_va, cause, flags, elr_va);
    } else {
        todo!("send upcall exception info");
    }
    crate::interrupt::set(false);
    crate::thread::exit_kernel();
}

/// Initializes the exception vector table by writing the address of
/// the table to the Vector Base Address Register (VBAR).
pub fn init() {
    extern "C" {
        // MaybeUninit<T> is guaranteed to have the same size/alignment as T
        static __exception_vector_table: core::mem::MaybeUninit<u64>;
    }
    // Write virtual address of table to VBAR
    unsafe {
        VBAR_EL1.set(__exception_vector_table.as_ptr() as u64);
    }
}
