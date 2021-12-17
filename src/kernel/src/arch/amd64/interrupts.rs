use crate::{arch::lapic, interrupt::Destination, processor::current_processor};

use super::desctables;

use x86::current::rflags::RFlags;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
lazy_static::lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        unsafe{
        idt.double_fault.set_handler_fn(double_fault_handler).set_stack_index(desctables::DOUBLE_FAULT_IST_INDEX);
        }
        idt.page_fault.set_handler_fn(page_fault_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.bound_range_exceeded.set_handler_fn(bounds_range_exceeded_handler);
        idt.debug.set_handler_fn(debug_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.machine_check.set_handler_fn(machine_check_handler);
        idt.non_maskable_interrupt.set_handler_fn(nmi_handler);
        idt.overflow.set_handler_fn(overflow_handler);
        idt.security_exception.set_handler_fn(security_exception_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_handler);
        idt.virtualization.set_handler_fn(virtualization_handler);
        idt.x87_floating_point.set_handler_fn(floating_point_handler);
        /* TODO: priv levels for all of these? */
        idt.slice_mut(32..)[0].set_handler_fn(irq_entry_0);
        idt.slice_mut(32 + 1..)[0].set_handler_fn(irq_entry_1);
        idt.slice_mut(32 + 2..)[0].set_handler_fn(irq_entry_2);
        idt.slice_mut(32 + 3..)[0].set_handler_fn(irq_entry_3);
        idt.slice_mut(32 + 4..)[0].set_handler_fn(irq_entry_4);
        idt.slice_mut(32 + 5..)[0].set_handler_fn(irq_entry_5);
        idt.slice_mut(32 + 6..)[0].set_handler_fn(irq_entry_6);
        idt.slice_mut(32 + 7..)[0].set_handler_fn(irq_entry_7);
        idt.slice_mut(32 + 8..)[0].set_handler_fn(irq_entry_8);
        idt.slice_mut(32 + 9..)[0].set_handler_fn(irq_entry_9);
        idt.slice_mut(32 + 10..)[0].set_handler_fn(irq_entry_10);
        idt.slice_mut(32 + 11..)[0].set_handler_fn(irq_entry_11);
        idt.slice_mut(32 + 12..)[0].set_handler_fn(irq_entry_12);
        idt.slice_mut(32 + 13..)[0].set_handler_fn(irq_entry_13);
        idt.slice_mut(32 + 14..)[0].set_handler_fn(irq_entry_14);
        idt.slice_mut(32 + 15..)[0].set_handler_fn(irq_entry_15);

        idt.slice_mut(240..)[0].set_handler_fn(irq_entry_240);
        idt.slice_mut(241..)[0].set_handler_fn(irq_entry_241);
        idt.slice_mut(242..)[0].set_handler_fn(irq_entry_242);
        idt.slice_mut(243..)[0].set_handler_fn(irq_entry_243);
        idt.slice_mut(244..)[0].set_handler_fn(irq_entry_244);
        idt.slice_mut(245..)[0].set_handler_fn(irq_entry_245);
        idt.slice_mut(246..)[0].set_handler_fn(irq_entry_246);
        idt.slice_mut(247..)[0].set_handler_fn(irq_entry_247);
        idt.slice_mut(248..)[0].set_handler_fn(irq_entry_248);
        idt.slice_mut(249..)[0].set_handler_fn(irq_entry_249);
        idt.slice_mut(250..)[0].set_handler_fn(irq_entry_250);
        idt.slice_mut(251..)[0].set_handler_fn(irq_entry_251);
        idt.slice_mut(252..)[0].set_handler_fn(irq_entry_252);
        idt.slice_mut(253..)[0].set_handler_fn(irq_entry_253);
        idt.slice_mut(254..)[0].set_handler_fn(irq_entry_254);
        idt.slice_mut(255..)[0].set_handler_fn(irq_entry_255);
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

pub enum InterProcessorInterrupt {
    Reschedule = 241,
}

fn irq_common(_stack_frame: InterruptStackFrame, irq: u16) {
    if irq == 0 {
        unsafe {
            if current_processor().is_bsp() {
                lapic::send_ipi(Destination::AllButSelf, 32);
            }
        }
        super::pit::timer_interrupt();
    }
    if irq >= 240 {
        lapic::lapic_interrupt(irq);
    }
    lapic::eoi();
    crate::interrupt::post_interrupt();
}

macro_rules! irq {
    ($name:ident, $vector:expr) => {
        extern "x86-interrupt" fn $name(stack_frame: InterruptStackFrame) {
            irq_common(stack_frame, $vector);
        }
    };
}

irq!(irq_entry_0, 0);
irq!(irq_entry_1, 1);
irq!(irq_entry_2, 2);
irq!(irq_entry_3, 3);
irq!(irq_entry_4, 4);
irq!(irq_entry_5, 5);
irq!(irq_entry_6, 6);
irq!(irq_entry_7, 7);
irq!(irq_entry_8, 8);
irq!(irq_entry_9, 9);
irq!(irq_entry_10, 10);
irq!(irq_entry_11, 11);
irq!(irq_entry_12, 12);
irq!(irq_entry_13, 13);
irq!(irq_entry_14, 14);
irq!(irq_entry_15, 15);
irq!(irq_entry_240, 240);
irq!(irq_entry_241, 241);
irq!(irq_entry_242, 242);
irq!(irq_entry_243, 243);
irq!(irq_entry_244, 244);
irq!(irq_entry_245, 245);
irq!(irq_entry_246, 246);
irq!(irq_entry_247, 247);
irq!(irq_entry_248, 248);
irq!(irq_entry_249, 249);
irq!(irq_entry_250, 250);
irq!(irq_entry_251, 251);
irq!(irq_entry_252, 252);
irq!(irq_entry_253, 253);
irq!(irq_entry_254, 254);
irq!(irq_entry_255, 255);

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;
    emerglogln!(
        "page fault: err {:?} cr2 {:?}\n{:#?}",
        error_code,
        Cr2::read(),
        stack_frame
    );
    let rbp = x86::bits64::registers::rbp();
    let rbp = unsafe { *(rbp as *const u64) };
    crate::panic::backtrace(
        true,
        Some(backtracer_core::EntryPoint::new(
            rbp,
            stack_frame.stack_pointer.as_u64(),
            stack_frame.instruction_pointer.as_u64(),
        )),
    );
    panic!("pf");
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    logln!("[cpu] breakpoint: {:#?}", stack_frame)
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) -> ! {
    let rbp = x86::bits64::registers::rbp();
    let rbp = unsafe { *(rbp as *const u64) };
    crate::panic::backtrace(
        true,
        Some(backtracer_core::EntryPoint::new(
            rbp,
            stack_frame.stack_pointer.as_u64(),
            stack_frame.instruction_pointer.as_u64(),
        )),
    );
    panic!(
        "exception: double fault: err {}\n{:#?}",
        error_code, stack_frame
    );
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: invalid opcode: {:#?}", stack_frame)
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    panic!("exception: GPF: {:#?}, err: {:x}", stack_frame, error_code)
}

extern "x86-interrupt" fn floating_point_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn stack_segment_handler(stack_frame: InterruptStackFrame, _err: u64) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, _err: u64) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn alignment_check_handler(stack_frame: InterruptStackFrame, _err: u64) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn nmi_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn security_exception_handler(stack_frame: InterruptStackFrame, _err: u64) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn virtualization_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn bounds_range_exceeded_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    panic!("exception: {:#?}", stack_frame);
}
extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, _err: u64) {
    panic!("exception: {:#?}", stack_frame);
}

pub fn disable() -> bool {
    let mut flags = x86::bits64::rflags::read();
    let old_if = flags.contains(RFlags::FLAGS_IF);
    flags.set(RFlags::FLAGS_IF, false);
    x86::bits64::rflags::set(flags);
    old_if
}

pub fn set(state: bool) {
    let mut flags = x86::bits64::rflags::read();
    flags.set(RFlags::FLAGS_IF, state);
    x86::bits64::rflags::set(flags);
}
