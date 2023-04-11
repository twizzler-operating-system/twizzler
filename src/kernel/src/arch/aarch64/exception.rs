/// ARMv8 exception handling
///
/// Configuration of the exception vector table, and 
/// Handling of synchronous (internal) exceptions.
/// External interrupts handled in [interrupts.rs]
///
/// We currently do not handle nested exceptions.

use core::arch::{asm, global_asm};
use core::fmt::{Display, Formatter, Result};
use arm64::registers::{VBAR_EL1, ESR_EL1};
use registers::{
    registers::InMemoryRegister,
    interfaces::{Readable, Writeable},
};

global_asm!(r#"
/// Exception Vector Table Definition for EL1 (Kernel)

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
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
.align {VECTOR_ALIGNMENT}

// Handling an exception from a Lower EL that is running in AArch64. 
// Lower meaning lower priviledge (EL0/user). Basically do we handle
// exceptions that occur in userspace (syscalls, etc.).
b default_exception_handler
.align {VECTOR_ALIGNMENT}
b default_exception_handler
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

/// Registers that are save/resored when handling an exception
#[derive(Debug)]
struct ExceptionContext {
    x0: u64,
    x1: u64,
    x2: u64,
    x3: u64,
    x4: u64,
    x5: u64,
    x6: u64,
    x7: u64,
    x8: u64,
    x9: u64,
    x10: u64,
    x11: u64,
    x12: u64,
    x13: u64,
    x14: u64,
    x15: u64,
    x16: u64,
    x17: u64,
    x18: u64,
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
    x30: u64,
}

impl Display for ExceptionContext {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        writeln!(f, "ExceptionContext (registers x0-x30):")?;
        writeln!(f, "\tx0:  {:#018x} x1:  {:#018x} x2:  {:#018x} x3:  {:#018x}", self.x0, self.x1, self.x2, self.x3)?;
        writeln!(f, "\tx4:  {:#018x} x5:  {:#018x} x6:  {:#018x} x7:  {:#018x}", self.x4, self.x5, self.x6, self.x7)?;
        writeln!(f, "\tx8:  {:#018x} x9:  {:#018x} x10: {:#018x} x11: {:#018x}", self.x8, self.x9, self.x10, self.x11)?;
        writeln!(f, "\tx12: {:#018x} x13: {:#018x} x14: {:#018x} x15: {:#018x}", self.x12, self.x13, self.x14, self.x15)?;
        writeln!(f, "\tx16: {:#018x} x17: {:#018x} x18: {:#018x} x19: {:#018x}", self.x16, self.x17, self.x18, self.x19)?;
        writeln!(f, "\tx20: {:#018x} x21: {:#018x} x22: {:#018x} x23: {:#018x}", self.x20, self.x21, self.x22, self.x23)?;
        writeln!(f, "\tx24: {:#018x} x25: {:#018x} x26: {:#018x} x27: {:#018x}", self.x24, self.x25, self.x26, self.x27)?;
        writeln!(f, "\tx28: {:#018x} x29: {:#018x} x30: {:#018x} ", self.x28, self.x29, self.x30)
    }
}

/// macro creates a high level exception handler
/// to be used in the exception vector table.
/// saves/restores regs and calls the specified handler
macro_rules! exception_handler {
    ($name:ident, $handler:ident) => {
        #[naked]
        #[no_mangle]
        unsafe extern "C" fn $name() {
            asm!(
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
                "str x30, [sp, #16 * 15]",
                // move stack pointer of last frame as an argument
                "mov x0, sp",
                // go to exception handler (overwrites x30)
                "bl {handler}",
                // restore all general purpose registers (x0-x30)
                // pop registers off of the stack
                "ldr x30, [sp, #16 * 15]",
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
                options(noreturn)
            )
        }
    };
}

// Default exception handler simply prints out 
// verbose debug information to the kernel console.
exception_handler!(default_exception_handler, debug_handler);

/// Exception handler prints information about the
/// stack frame that generated the exception and other
/// useful system register state. Then it panics.
fn debug_handler(ctx: &mut ExceptionContext) {
    // read of raw value for ESR
    let esr = ESR_EL1.get();
    // print reason for exception (syndrome register)
    emerglogln!("[kernel::exception] Exception Syndrome Register (ESR) value: {:#x}", esr);
    // print decoding information
    emerglogln!("[kernel::exception] decoding ESR");
    // ec: exception class
    let esr_reg: InMemoryRegister<u64, ESR_EL1::Register> = InMemoryRegister::new(esr);
    emerglogln!("ESR[31:26] = {:#x} ==> EC (Exception Class)", esr_reg.read(ESR_EL1::EC));
    emerglogln!("\t{}", 
        match esr_reg.read_as_enum(ESR_EL1::EC) {
            Some(ESR_EL1::EC::Value::SVC64) => "SVC instruction execution in AArch64 state.",
            Some(ESR_EL1::EC::Value::Unknown) | _ => "Unknown reason.",
        }
    );
    // iss: syndrome
    emerglogln!("ESR[24:0] = {:#x} ==> ISS (Instruction Specific Syndrome)", esr_reg.read(ESR_EL1::ISS));
    
    // TODO:
    // print faulting address (ELR/FAR)
    // print other system registers: PSTATE/SPSR

    // print registers
    emerglog!("[kernel::exception] dumping register state: {}", ctx);

    panic!("caught unhandled exception!!!")
}

/// Initializes the exception vector table by writing the address of 
/// the table to the Vector Base Address Register (VBAR).
pub fn init() {
    extern {
        // MaybeUninit<T> is guaranteed to have the same size/alignment as T
        static __exception_vector_table: core::mem::MaybeUninit<u64>;
    }
    // Write virtual address of table to VBAR
    unsafe { 
        emerglogln!("[kernel::exception] setting vbar to address {:p}", __exception_vector_table.as_ptr());
        VBAR_EL1.set(__exception_vector_table.as_ptr() as u64); 
    }
}
