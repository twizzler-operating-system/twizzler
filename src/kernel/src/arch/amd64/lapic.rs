use core::intrinsics::unlikely;

use x86::{
    io::outb,
    segmentation::{BuildDescriptor, Descriptor, SegmentDescriptorBuilder},
};

use crate::{
    arch::memory::phys_to_virt,
    clock::Nanoseconds,
    interrupt::{self, Destination},
    memory::{PhysAddr, VirtAddr},
    processor,
};

static mut LAPIC_ADDR: u64 = 0;

const LAPIC_ID: u32 = 0x20;
const LAPIC_VER: u32 = 0x30;
const LAPIC_TPR: u32 = 0x0080;
const LAPIC_EOI: u32 = 0x00b0;
const LAPIC_LDR: u32 = 0x00d0;
const LAPIC_DFR: u32 = 0x00e0;
const LAPIC_SVR: u32 = 0x00f0;
const LAPIC_ESR: u32 = 0x0280;
const LAPIC_ICRLO: u32 = 0x0300;
const LAPIC_ICRHI: u32 = 0x0310;
const LAPIC_TIMER: u32 = 0x0320;
const LAPIC_PCINT: u32 = 0x0340;
const LAPIC_LINT0: u32 = 0x0350;
const LAPIC_LINT1: u32 = 0x0360;
const LAPIC_ERROR: u32 = 0x0370;
const LAPIC_TICR: u32 = 0x0380;
const LAPIC_TDCR: u32 = 0x03e0;

const LAPIC_ICRLO_INIT: u32 = 0x0500;
const LAPIC_ICRLO_STARTUP: u32 = 0x0600;
const LAPIC_ICRLO_LEVEL: u32 = 0x8000;
const LAPIC_ICRLO_ASSERT: u32 = 0x4000;
const LAPIC_ICRLO_STATUS_PEND: u32 = 0x1000;

unsafe fn read_lapic(reg: u32) -> u32 {
    core::ptr::read_volatile((LAPIC_ADDR + reg as u64) as *const u32)
}

unsafe fn write_lapic(reg: u32, val: u32) {
    core::ptr::write_volatile((LAPIC_ADDR + reg as u64) as *mut u32, val);
    core::ptr::read_volatile((LAPIC_ADDR + LAPIC_ID as u64) as *const u32);
}

static mut FREQ_MHZ: u64 = 0;
pub fn get_speeds() {
    let cpuid = x86::cpuid::CpuId::new();
    let features = cpuid.get_feature_info().unwrap();
    if !features.has_tsc_deadline() {
        unimplemented!("APIC without TSC deadline");
    }
    if !cpuid
        .get_advanced_power_mgmt_info()
        .unwrap()
        .has_invariant_tsc()
    {
        unimplemented!("support for non-invariant tsc");
    }
    let tsc_speed_info = cpuid.get_tsc_info();
    if let Some(speed) = tsc_speed_info.map(|info| info.tsc_frequency()).flatten() {
        unsafe { FREQ_MHZ = speed / 1000000 };
        return;
    }
    if let Some(cpu_speed_info) = cpuid.get_processor_frequency_info() {
        unsafe { FREQ_MHZ = cpu_speed_info.processor_base_frequency() as u64 };
        if cpu_speed_info.processor_base_frequency() > 0 {
            return;
        }
    }
    if let Some(speed) = cpuid
        .get_hypervisor_info()
        .map(|info| info.tsc_frequency())
        .flatten()
    {
        unsafe { FREQ_MHZ = speed as u64 / 1000 };
        return;
    }
    unsafe { FREQ_MHZ = 2000 };
    unimplemented!("measure TSC freq");
}

pub fn init(bsp: bool) {
    if bsp {
        unsafe {
            let apic_base = x86::msr::rdmsr(x86::msr::APIC_BASE) as u32;
            LAPIC_ADDR = phys_to_virt(PhysAddr::new((apic_base & 0xffff0000) as u64)).as_u64();
        }
        get_speeds();
    }

    unsafe {
        write_lapic(LAPIC_SVR, 0x100 | 0xff);

        write_lapic(LAPIC_TIMER, 0x10000 | 32);
        write_lapic(LAPIC_TICR, 10000000);
        write_lapic(LAPIC_LINT0, 0x10000);
        write_lapic(LAPIC_LINT1, 0x10000);

        write_lapic(LAPIC_ERROR, 0xfe);

        write_lapic(LAPIC_ESR, 0);
        write_lapic(LAPIC_ESR, 0);

        write_lapic(LAPIC_DFR, 0xffffffff);
        write_lapic(LAPIC_LDR, 1 << 24);

        write_lapic(LAPIC_EOI, 0);
        write_lapic(LAPIC_TPR, 0);
    }
}

pub fn eoi() {
    unsafe {
        write_lapic(LAPIC_EOI, 0);
    }
}

pub fn lapic_interrupt(irq: u16) {
    match irq {
        0xfe => panic!("LAPIC error"),
        0xf0 => crate::clock::oneshot_clock_hardtick(),
        0xf1 => crate::sched::schedule_resched(),
        _ => unimplemented!(),
    }
}

const LAPIC_TIMER_DEADLINE: u32 = 0x40000;
pub fn schedule_oneshot_tick(time: Nanoseconds) {
    let old = interrupt::disable();
    unsafe {
        let time = read_monotonic_nanoseconds() + time;
        let deadline = (time / 1000) * FREQ_MHZ;
        write_lapic(LAPIC_TIMER, 240 | LAPIC_TIMER_DEADLINE);
        x86::msr::wrmsr(x86::msr::IA32_TSC_DEADLINE, deadline);
    }
    interrupt::set(old);
}

pub fn read_monotonic_nanoseconds() -> Nanoseconds {
    // TODO: should we use rdtsc or rdtscp here? (the latter will require a cpuid check once (only
    // once, cache the result))
    let tsc = unsafe { x86::time::rdtsc() };
    let f = unsafe { FREQ_MHZ };
    if unlikely(f == 0) {
        panic!("cannot read nanoseconds before TSC calibration");
    }
    (tsc * 1000u64) / f
}

#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code16() {
    asm!(
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
        options(noreturn)
    )
}
#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code32() {
    asm!(
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
        options(noreturn)
    )
}

#[naked]
#[allow(named_asm_labels)]
unsafe extern "C" fn trampoline_entry_code64() {
    asm!(
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
        options(noreturn)
    )
}

#[inline(never)]
#[no_mangle]
extern "C" fn trampoline_main_entry(id: u32, tcb: u64, stack_base: u64) -> ! {
    rust_entry_secondary(id, tcb, stack_base);
}

#[inline(never)]
fn rust_entry_secondary(id: u32, tcb: u64, stack_base: u64) -> ! {
    crate::processor::secondary_entry(id, VirtAddr::new(tcb), stack_base as *mut u8);
}

pub fn send_ipi(dest: Destination, vector: u32) {
    let (dest_short, dest_val) = match dest {
        Destination::Single(id) => (0, id << 24),
        Destination::Bsp => (0, processor::current_processor().bsp_id() << 24),
        Destination::All => (2, 0),
        Destination::AllButSelf => (3, 0),
        _ => todo!(),
    };
    unsafe {
        write_lapic(LAPIC_ICRHI, dest_val);
        write_lapic(LAPIC_ICRLO, vector | dest_short << 18);

        while read_lapic(LAPIC_ICRLO) & LAPIC_ICRLO_STATUS_PEND != 0 {
            asm!("pause")
        }
    }
}

const TRAMPOLINE_ENTRY16: u32 = 0x7000;
const TRAMPOLINE_ENTRY32: u32 = 0x7100;
const TRAMPOLINE_ENTRY64: u32 = 0x7200;
/// Start up a CPU.
/// # Safety
/// The tcb_base and kernel stack must both be valid memory regions for each thing.
pub unsafe fn poke_cpu(cpu: u32, tcb_base: VirtAddr, kernel_stack: *mut u8) {
    outb(0x70, 0xf);
    super::pit::wait_ns(100);
    outb(0x71, 0x0a);
    super::pit::wait_ns(100);

    let phys_mem_offset = phys_to_virt(PhysAddr::new(0)).as_u64();

    let bios_reset = (phys_mem_offset + 0x467) as *mut u32;
    *bios_reset = (TRAMPOLINE_ENTRY16 & 0xff000) << 12;
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
    *tcb = tcb_base.as_u64();
    assert!(*pagetables >> 32 == 0);
    asm!("mfence");

    write_lapic(LAPIC_ESR, 0);
    send_ipi(
        Destination::Single(cpu),
        LAPIC_ICRLO_INIT | LAPIC_ICRLO_LEVEL | LAPIC_ICRLO_ASSERT,
    );
    super::pit::wait_ns(100000);

    send_ipi(
        Destination::Single(cpu),
        LAPIC_ICRLO_INIT | LAPIC_ICRLO_LEVEL,
    );
    super::pit::wait_ns(100000);

    for _ in 0..3 {
        send_ipi(
            Destination::Single(cpu),
            LAPIC_ICRLO_STARTUP | ((TRAMPOLINE_ENTRY16 >> 12) & 0xff),
        );
        super::pit::wait_ns(100000);
    }
}
