use x86::{
    io::outb,
    segmentation::{BuildDescriptor, Descriptor, SegmentDescriptorBuilder},
};

use crate::{
    arch::{
        amd64::{
            apic::local::{
                get_lapic, LAPIC_ICRLO_ASSERT, LAPIC_ICRLO_INIT, LAPIC_ICRLO_LEVEL,
                LAPIC_ICRLO_STARTUP,
            },
            pit,
        },
        memory::phys_to_virt,
        send_ipi,
    },
    interrupt::Destination,
    memory::{PhysAddr, VirtAddr},
};

// TODO: cleanup magic numbers.

#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code16() {
    core::arch::naked_asm!(
        ".code16gcc",
        "mov ax, [0x6f18]",
        "cli",
        "xor ax, ax",
        "mov ds, ax",
        "mov es, ax",
        "mov ss, ax",
        "mov gs, ax",
        "mov fs, ax",
        "cld",
        "lgdt [0x6f18]",
        "mov eax, cr0",
        "or eax, 1",
        "mov cr0, eax",
        ".byte 0xea",
        ".byte 0x00",
        ".byte 0x71",
        ".byte 0x08",
        ".byte 0x00",
    )
}
#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code32() {
    core::arch::naked_asm!(
        ".code32",
        "mov ax, 16",
        "mov ds, ax",
        "mov ss, ax",
        "mov es, ax",
        "xor ax, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov eax, cr4",
        "bts eax, 5",
        "bts eax, 7",
        "mov cr4, eax",
        "mov ecx, 0xc0000080",
        "rdmsr",
        "bts eax, 8",
        "bts eax, 11",
        "wrmsr",
        "mov eax, [0x6f40]",
        "mov cr3, eax",
        "mov eax, cr0",
        "xor eax, eax",
        "bts eax, 31",
        "bts eax, 0",
        "mov cr0, eax",
        "lgdt [0x6f68]",
        ".byte 0xea",
        ".byte 0x00",
        ".byte 0x72",
        ".byte 0x00",
        ".byte 0x00",
        ".byte 0x08",
        ".byte 0x00",
    )
}

#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code64() {
    core::arch::naked_asm!(
        "lgdt [0x6f68]",
        "mov ax, 0x10",
        "mov ds, ax",
        "mov es, ax",
        "mov fs, ax",
        "mov gs, ax",
        "mov ss, ax",
        "xor rbp, rbp",
        "mov rsp, [0x6f48]",
        "mov rax, [0x6fa0]",
        "mov edi, [0x6fa8]",
        "mov rsi, [0x6fb0]",
        "mov rdx, rsp",
        "add rsp, {stack_size}",
        "call rax",
        "ud2",
        stack_size = const(crate::processor::KERNEL_STACK_SIZE),
    )
}

#[inline(never)]
#[no_mangle]
extern "C" fn trampoline_main_entry(id: u32, tcb: u64, stack_base: u64) -> ! {
    rust_entry_secondary(id, tcb, stack_base);
}

#[inline(never)]
fn rust_entry_secondary(id: u32, tcb: u64, stack_base: u64) -> ! {
    crate::processor::secondary_entry(id, VirtAddr::new(tcb).unwrap(), stack_base as *mut u8);
}

const TRAMPOLINE_ENTRY16: u32 = 0x7000;
const TRAMPOLINE_ENTRY32: u32 = 0x7100;
const TRAMPOLINE_ENTRY64: u32 = 0x7200;
/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(cpu: u32, tcb_base: VirtAddr, kernel_stack: *mut u8) {
    outb(0x70, 0xf);
    pit::wait_ns(100);
    outb(0x71, 0x0a);
    pit::wait_ns(100);

    let phys_mem_offset = phys_to_virt(PhysAddr::new(0).unwrap()).raw();

    let bios_reset = (phys_mem_offset + 0x467) as *mut u32;
    bios_reset.write_unaligned((TRAMPOLINE_ENTRY16 & 0xff000) << 12);
    let trampoline16 = (phys_mem_offset + TRAMPOLINE_ENTRY16 as u64) as *mut u8;
    trampoline16.copy_from_nonoverlapping(trampoline_entry_code16 as *const u8, 0x100);
    let trampoline32 = (TRAMPOLINE_ENTRY32 as u64) as *mut u8;
    trampoline32.copy_from_nonoverlapping(trampoline_entry_code32 as *const u8, 0x100);
    let trampoline64 = (TRAMPOLINE_ENTRY64 as u64) as *mut u8;
    trampoline64.copy_from_nonoverlapping(trampoline_entry_code64 as *const u8, 0x100);

    let gdt_phys = 0x6f00 as *mut x86::segmentation::Descriptor;
    let gdt = (0x6f00 + phys_mem_offset) as *mut x86::segmentation::Descriptor;
    let gdt64_phys = 0x6f50 as *mut x86::segmentation::Descriptor;
    let gdt64 = (0x6f50 + phys_mem_offset) as *mut x86::segmentation::Descriptor;
    let mut code_seg: Descriptor = x86::segmentation::DescriptorBuilder::code_descriptor(
        0,
        0xfffff,
        x86::segmentation::CodeSegmentType::ExecuteRead,
    )
    .limit_granularity_4kb()
    .present()
    .finish();
    let mut data_seg: Descriptor = x86::segmentation::DescriptorBuilder::data_descriptor(
        0,
        0xfffff,
        x86::segmentation::DataSegmentType::ReadWrite,
    )
    .limit_granularity_4kb()
    .present()
    .finish();
    let mut code64_seg = code_seg;
    code64_seg.set_l();
    let mut data64_seg = data_seg;
    data64_seg.set_l();
    code_seg.set_db();
    data_seg.set_db();
    gdt.write(x86::segmentation::Descriptor::default());
    gdt.add(1).write(code_seg);
    gdt.add(2).write(data_seg);
    gdt64.write(x86::segmentation::Descriptor::default());
    gdt64.add(1).write(code64_seg);
    gdt64.add(2).write(data64_seg);
    let gdtp = (0x6f18 + phys_mem_offset)
        as *mut x86::dtables::DescriptorTablePointer<x86::segmentation::Descriptor>;
    gdtp.write(x86::dtables::DescriptorTablePointer::new_from_slice(
        core::slice::from_raw_parts(gdt_phys, 3),
    ));
    let gdtp64 = (0x6f68 + phys_mem_offset)
        as *mut x86::dtables::DescriptorTablePointer<x86::segmentation::Descriptor>;
    gdtp64.write(x86::dtables::DescriptorTablePointer::new_from_slice(
        core::slice::from_raw_parts(gdt64_phys, 3),
    ));

    let pagetables = (0x6f40 + phys_mem_offset) as *mut u64;
    *pagetables = x86::controlregs::cr3();
    let stack = (0x6f48 + phys_mem_offset) as *mut u64;
    *stack = kernel_stack as u64;
    let entry = (0x6fa0 + phys_mem_offset) as *mut u64;
    *entry = trampoline_main_entry as *const u8 as u64;
    let id = (0x6fa8 + phys_mem_offset) as *mut u32;
    *id = cpu;
    let tcb = (0x6fb0 + phys_mem_offset) as *mut u64;
    *tcb = tcb_base.raw();
    assert!(*pagetables >> 32 == 0);
    core::arch::asm!("mfence");

    get_lapic().clear_err();
    send_ipi(
        Destination::Single(cpu),
        LAPIC_ICRLO_INIT | LAPIC_ICRLO_LEVEL | LAPIC_ICRLO_ASSERT,
    );
    pit::wait_ns(100000);

    send_ipi(
        Destination::Single(cpu),
        LAPIC_ICRLO_INIT | LAPIC_ICRLO_LEVEL,
    );
    pit::wait_ns(100000);

    for _ in 0..3 {
        send_ipi(
            Destination::Single(cpu),
            LAPIC_ICRLO_STARTUP | ((TRAMPOLINE_ENTRY16 >> 12) & 0xff),
        );
        pit::wait_ns(100000);
    }
}
