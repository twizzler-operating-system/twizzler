use x86::current::rflags::RFlags;
use x86_64::{instructions::segmentation::Segment64, VirtAddr};

use crate::{
    arch::lapic,
    interrupt::Destination,
    memory::fault::{PageFaultCause, PageFaultFlags},
    processor::current_processor,
    thread::current_thread_ref,
};

struct IsrContext {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rsi: u64,
    rdi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    err: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

impl core::fmt::Debug for IsrContext {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let rip = self.rip as *const u8;
        f.debug_struct("IsrContext")
            .field("r15", &self.r15)
            .field("r14", &self.r14)
            .field("r13", &self.r13)
            .field("r12", &self.r12)
            .field("r11", &self.r11)
            .field("r10", &self.r10)
            .field("r9", &self.r9)
            .field("r8", &self.r8)
            .field("rbp", &self.rbp)
            .field("rsi", &self.rsi)
            .field("rdi", &self.rdi)
            .field("rdx", &self.rdx)
            .field("rcx", &self.rcx)
            .field("rbx", &self.rbx)
            .field("rax", &self.rax)
            .field("err", &self.err)
            .field("rip", &rip)
            .field("cs", &self.cs)
            .field("rflags", &self.rflags)
            .field("rsp", &self.rsp)
            .field("ss", &self.ss)
            .finish()
    }
}

#[no_mangle]
unsafe extern "C" fn common_handler_entry(
    ctx: *mut IsrContext,
    number: u64,
    user: u64,
    kernel_fs: u64,
) {
    let user = user != 0;
    if user {
        x86_64::registers::segmentation::FS::write_base(VirtAddr::new(kernel_fs));
        x86::msr::wrmsr(x86::msr::IA32_FS_BASE, kernel_fs);
    }
    generic_isr_handler(ctx, number, user);

    if user {
        let t = current_thread_ref().unwrap();
        let user_fs = t.arch.user_fs;
        x86_64::registers::segmentation::FS::write_base(VirtAddr::new(user_fs));
        x86::msr::wrmsr(x86::msr::IA32_FS_BASE, user_fs);
    }
}

#[no_mangle]
#[naked]
pub unsafe extern "C" fn kernel_interrupt() {
    asm!("mov qword ptr [rsp - 8], 0", "sub rsp, 8", "xor rdx, rdx", "call {common}", "add rsp, 8", "jmp return_from_interrupt", common = sym common_handler_entry, options(noreturn));
}

#[allow(named_asm_labels)]
#[no_mangle]
#[naked]
pub unsafe extern "C" fn user_interrupt() {
    asm!(
        "swapgs",
        "mov rcx, gs:8",
        "mov rdx, 1",
        "sub rsp, 8",
        "call {common}", 
        "add rsp, 8",
        "swapgs",
        "jmp return_from_interrupt", common = sym common_handler_entry, options(noreturn));
}

#[no_mangle]
#[naked]
pub unsafe extern "C" fn return_from_interrupt() {
    asm!(
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12 ",
        "pop r11 ",
        "pop r10",
        "pop r9 ",
        "pop r8 ",
        "pop rbp",
        "pop rsi",
        "pop rdi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        "add rsp, 8",
        "iretq",
        options(noreturn)
    );
}

macro_rules! interrupt {
    ($name:ident, $num:expr) => {
        #[naked]
        #[allow(named_asm_labels)]
        unsafe extern "C" fn $name() {
            asm!(
                "mov qword ptr [rsp - 8], 0",
                "sub rsp, 8",
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rdi",
                "push rsi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "test qword ptr [rsp + 160], 3",
                "mov rdi, rsp",
                concat!("mov rsi, ", $num),
                "jz kernel_interrupt",
                "jmp user_interrupt",
                options(noreturn)
            )
        }
    };
}
macro_rules! interrupt_err {
    ($name:ident, $num:expr) => {
        #[naked]
        #[allow(named_asm_labels)]
        unsafe extern "C" fn $name() {
            asm!(
                "push rax",
                "push rbx",
                "push rcx",
                "push rdx",
                "push rdi",
                "push rsi",
                "push rbp",
                "push r8",
                "push r9",
                "push r10",
                "push r11",
                "push r12",
                "push r13",
                "push r14",
                "push r15",
                "test qword ptr [rsp + 160], 3",
                "mov rdi, rsp",
                concat!("mov rsi, ", $num),
                "jz kernel_interrupt",
                "jmp user_interrupt",
                options(noreturn)
            )
        }
    };
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct IDTEntry {
    offset_low: u16,
    seg: u16,
    flags: u16,
    offset_med: u16,
    offset_high: u32,
    resv: u32,
}

impl IDTEntry {
    fn new(user: bool, ist: u16, addr: u64) -> Self {
        assert!(ist < 7);
        let flags: u16 = (1 << 15) | if user { 3 << 13 } else { 0 } | ist | 0xE << 8;
        Self {
            offset_low: (addr & 0xffff) as u16,
            offset_med: ((addr >> 16) & 0xffff) as u16,
            offset_high: ((addr >> 32) & 0xffffffff) as u32,
            seg: 0x08,
            resv: 0,
            flags,
        }
    }

    const fn missing() -> Self {
        Self {
            offset_low: 0,
            offset_med: 0,
            offset_high: 0,
            flags: 0,
            seg: 0,
            resv: 0,
        }
    }
}

#[repr(align(16), C)]
struct InterruptDescriptorTable {
    idt: [IDTEntry; 256],
}

#[repr(C, packed)]
struct InterruptDescriptorTablePointer {
    limit: u16,
    base: u64,
}

impl InterruptDescriptorTable {
    const fn new() -> Self {
        const MISSING: IDTEntry = IDTEntry::missing();
        Self {
            idt: [MISSING; 256],
        }
    }

    fn set_handler(
        &mut self,
        nr: usize,
        handler: unsafe extern "C" fn(),
        user: bool,
        ist: Option<usize>,
    ) {
        self.idt[nr] = IDTEntry::new(
            user,
            ist.map_or(0, |i| i + 1) as u16,
            handler as usize as u64,
        );
    }

    unsafe fn load(&self) {
        let ptr = self.idt.as_ptr();
        let idtp = InterruptDescriptorTablePointer {
            limit: (core::mem::size_of::<Self>() - 1) as u16,
            base: ptr as u64,
        };

        asm!("lidt [{}]", in(reg) &idtp, options(readonly, nostack, preserves_flags));
    }
}

#[derive(Debug, Clone, Copy)]
#[repr(u64)]
enum Exception {
    DivideError = 0,
    Debug,
    NonMaskableInterrupt,
    Breakpoint,
    OverflowError,
    BoundsRangeExceeded,
    InvalidOpcode,
    DeviceNotAvailable,
    DoubleFault,
    CoProcessorOverrun,
    InvalidTSS,
    SegmentNotPresent,
    StackSegmentFault,
    GeneralProtectionFault,
    PageFault,
    Reserved1,
    X87FloatingPoint,
    AligmentCheck,
    MachineCheck,
    SIMDFloatingPoint,
    Virtualization,
    ControlProtection,
    Reserved2_0,
    Reserved2_1,
    Reserved2_2,
    Reserved2_3,
    Reserved2_4,
    Reserved2_5,
    HypervisorInjection,
    VMMCommunication,
    Security,
    Reserved3,
}

impl Exception {
    fn as_idx(&self) -> usize {
        *self as usize
    }
}

pub enum InterProcessorInterrupt {
    Reschedule = 241,
}

fn num_as_exception(n: u64) -> Exception {
    assert!(n < 32);
    unsafe { core::intrinsics::transmute(n) }
}

fn generic_isr_handler(ctx: *mut IsrContext, number: u64, _user: bool) {
    assert!(!disable());
    let ctx = unsafe { ctx.as_mut().unwrap() };
    if number == Exception::DoubleFault as u64 || number == Exception::MachineCheck as u64 {
        /* diverging */
        panic!(
            "caught diverging exception {:?}: {:#?}",
            num_as_exception(number),
            ctx
        );
    }

    if number == Exception::PageFault as u64 {
        let cr2 = unsafe { x86::controlregs::cr2() };
        let err = ctx.err;
        let cause = if err & (1 << 4) == 0 {
            if err & (1 << 1) == 0 {
                PageFaultCause::Read
            } else {
                PageFaultCause::Write
            }
        } else {
            PageFaultCause::InstructionFetch
        };
        let mut flags = PageFaultFlags::empty();
        if err & 1 != 0 {
            flags.insert(PageFaultFlags::PRESENT);
        }
        if err & (1 << 2) != 0 {
            flags.insert(PageFaultFlags::USER);
        }
        if err & (1 << 3) != 0 {
            flags.insert(PageFaultFlags::INVALID);
        }
        crate::thread::enter_kernel();
        crate::interrupt::set(true);
        crate::memory::fault::page_fault(
            VirtAddr::new(cr2 as u64),
            cause,
            flags,
            VirtAddr::new(ctx.rip),
        );
        crate::interrupt::set(false);
        crate::thread::exit_kernel();
    } else if number < 32 {
        panic!(
            "caught unhandled exception {:?}: {:#?}",
            num_as_exception(number),
            ctx
        );
    }
    if number == 32 {
        unsafe {
            if current_processor().is_bsp() {
                lapic::send_ipi(Destination::AllButSelf, 32);
            }
        }
        super::pit::timer_interrupt();
    }
    if number >= 240 {
        lapic::lapic_interrupt(number as u16);
    }
    lapic::eoi();
    crate::interrupt::post_interrupt();
}

interrupt!(divide_handler, 0);
interrupt!(debug_handler, 1);
interrupt!(nmi_handler, 2);
interrupt!(breakpoint_handler, 3);
interrupt!(overflow_handler, 4);
interrupt!(boundsrange_handler, 5);
interrupt!(invalid_opcode_handler, 6);
interrupt!(device_not_available_handler, 7);
interrupt_err!(double_fault_handler, 8);
interrupt!(coprocessor_overrun_handler, 9);
interrupt_err!(invalid_tss_handler, 10);
interrupt_err!(segment_not_present_handler, 11);
interrupt_err!(stack_segment_handler, 12);
interrupt_err!(gpf_handler, 13);
interrupt_err!(pagefault_handler, 14);
interrupt!(x87_floatingpoint_handler, 16);
interrupt_err!(alignment_check_handler, 17);
interrupt!(machine_check_handler, 18);
interrupt!(simd_floating_point_handler, 19);
interrupt!(virtualization_handler, 20);
interrupt_err!(control_protection_handler, 21);
interrupt!(hypervisor_injection_handler, 28);
interrupt_err!(vmm_communication_handler, 29);
interrupt_err!(security_handler, 30);

interrupt!(int32_handler, 32);
interrupt!(int33_handler, 33);
interrupt!(int34_handler, 34);
interrupt!(int35_handler, 35);
interrupt!(int36_handler, 36);
interrupt!(int37_handler, 37);
interrupt!(int38_handler, 38);
interrupt!(int39_handler, 39);
interrupt!(int40_handler, 40);
interrupt!(int41_handler, 41);
interrupt!(int42_handler, 42);
interrupt!(int43_handler, 43);
interrupt!(int44_handler, 44);
interrupt!(int45_handler, 45);
interrupt!(int46_handler, 46);
interrupt!(int47_handler, 47);
interrupt!(int48_handler, 48);
interrupt!(int49_handler, 49);
interrupt!(int50_handler, 50);
interrupt!(int51_handler, 51);
interrupt!(int52_handler, 52);
interrupt!(int53_handler, 53);
interrupt!(int54_handler, 54);
interrupt!(int55_handler, 55);
interrupt!(int56_handler, 56);
interrupt!(int57_handler, 57);
interrupt!(int58_handler, 58);
interrupt!(int59_handler, 59);
interrupt!(int60_handler, 60);
interrupt!(int61_handler, 61);
interrupt!(int62_handler, 62);
interrupt!(int63_handler, 63);
interrupt!(int64_handler, 64);
interrupt!(int65_handler, 65);
interrupt!(int66_handler, 66);
interrupt!(int67_handler, 67);
interrupt!(int68_handler, 68);
interrupt!(int69_handler, 69);
interrupt!(int70_handler, 70);
interrupt!(int71_handler, 71);
interrupt!(int72_handler, 72);
interrupt!(int73_handler, 73);
interrupt!(int74_handler, 74);
interrupt!(int75_handler, 75);
interrupt!(int76_handler, 76);
interrupt!(int77_handler, 77);
interrupt!(int78_handler, 78);
interrupt!(int79_handler, 79);
interrupt!(int80_handler, 80);
interrupt!(int81_handler, 81);
interrupt!(int82_handler, 82);
interrupt!(int83_handler, 83);
interrupt!(int84_handler, 84);
interrupt!(int85_handler, 85);
interrupt!(int86_handler, 86);
interrupt!(int87_handler, 87);
interrupt!(int88_handler, 88);
interrupt!(int89_handler, 89);
interrupt!(int90_handler, 90);
interrupt!(int91_handler, 91);
interrupt!(int92_handler, 92);
interrupt!(int93_handler, 93);
interrupt!(int94_handler, 94);
interrupt!(int95_handler, 95);
interrupt!(int96_handler, 96);
interrupt!(int97_handler, 97);
interrupt!(int98_handler, 98);
interrupt!(int99_handler, 99);
interrupt!(int100_handler, 100);
interrupt!(int101_handler, 101);
interrupt!(int102_handler, 102);
interrupt!(int103_handler, 103);
interrupt!(int104_handler, 104);
interrupt!(int105_handler, 105);
interrupt!(int106_handler, 106);
interrupt!(int107_handler, 107);
interrupt!(int108_handler, 108);
interrupt!(int109_handler, 109);
interrupt!(int110_handler, 110);
interrupt!(int111_handler, 111);
interrupt!(int112_handler, 112);
interrupt!(int113_handler, 113);
interrupt!(int114_handler, 114);
interrupt!(int115_handler, 115);
interrupt!(int116_handler, 116);
interrupt!(int117_handler, 117);
interrupt!(int118_handler, 118);
interrupt!(int119_handler, 119);
interrupt!(int120_handler, 120);
interrupt!(int121_handler, 121);
interrupt!(int122_handler, 122);
interrupt!(int123_handler, 123);
interrupt!(int124_handler, 124);
interrupt!(int125_handler, 125);
interrupt!(int126_handler, 126);
interrupt!(int127_handler, 127);
interrupt!(int128_handler, 128);
interrupt!(int129_handler, 129);
interrupt!(int130_handler, 130);
interrupt!(int131_handler, 131);
interrupt!(int132_handler, 132);
interrupt!(int133_handler, 133);
interrupt!(int134_handler, 134);
interrupt!(int135_handler, 135);
interrupt!(int136_handler, 136);
interrupt!(int137_handler, 137);
interrupt!(int138_handler, 138);
interrupt!(int139_handler, 139);
interrupt!(int140_handler, 140);
interrupt!(int141_handler, 141);
interrupt!(int142_handler, 142);
interrupt!(int143_handler, 143);
interrupt!(int144_handler, 144);
interrupt!(int145_handler, 145);
interrupt!(int146_handler, 146);
interrupt!(int147_handler, 147);
interrupt!(int148_handler, 148);
interrupt!(int149_handler, 149);
interrupt!(int150_handler, 150);
interrupt!(int151_handler, 151);
interrupt!(int152_handler, 152);
interrupt!(int153_handler, 153);
interrupt!(int154_handler, 154);
interrupt!(int155_handler, 155);
interrupt!(int156_handler, 156);
interrupt!(int157_handler, 157);
interrupt!(int158_handler, 158);
interrupt!(int159_handler, 159);
interrupt!(int160_handler, 160);
interrupt!(int161_handler, 161);
interrupt!(int162_handler, 162);
interrupt!(int163_handler, 163);
interrupt!(int164_handler, 164);
interrupt!(int165_handler, 165);
interrupt!(int166_handler, 166);
interrupt!(int167_handler, 167);
interrupt!(int168_handler, 168);
interrupt!(int169_handler, 169);
interrupt!(int170_handler, 170);
interrupt!(int171_handler, 171);
interrupt!(int172_handler, 172);
interrupt!(int173_handler, 173);
interrupt!(int174_handler, 174);
interrupt!(int175_handler, 175);
interrupt!(int176_handler, 176);
interrupt!(int177_handler, 177);
interrupt!(int178_handler, 178);
interrupt!(int179_handler, 179);
interrupt!(int180_handler, 180);
interrupt!(int181_handler, 181);
interrupt!(int182_handler, 182);
interrupt!(int183_handler, 183);
interrupt!(int184_handler, 184);
interrupt!(int185_handler, 185);
interrupt!(int186_handler, 186);
interrupt!(int187_handler, 187);
interrupt!(int188_handler, 188);
interrupt!(int189_handler, 189);
interrupt!(int190_handler, 190);
interrupt!(int191_handler, 191);
interrupt!(int192_handler, 192);
interrupt!(int193_handler, 193);
interrupt!(int194_handler, 194);
interrupt!(int195_handler, 195);
interrupt!(int196_handler, 196);
interrupt!(int197_handler, 197);
interrupt!(int198_handler, 198);
interrupt!(int199_handler, 199);
interrupt!(int200_handler, 200);
interrupt!(int201_handler, 201);
interrupt!(int202_handler, 202);
interrupt!(int203_handler, 203);
interrupt!(int204_handler, 204);
interrupt!(int205_handler, 205);
interrupt!(int206_handler, 206);
interrupt!(int207_handler, 207);
interrupt!(int208_handler, 208);
interrupt!(int209_handler, 209);
interrupt!(int210_handler, 210);
interrupt!(int211_handler, 211);
interrupt!(int212_handler, 212);
interrupt!(int213_handler, 213);
interrupt!(int214_handler, 214);
interrupt!(int215_handler, 215);
interrupt!(int216_handler, 216);
interrupt!(int217_handler, 217);
interrupt!(int218_handler, 218);
interrupt!(int219_handler, 219);
interrupt!(int220_handler, 220);
interrupt!(int221_handler, 221);
interrupt!(int222_handler, 222);
interrupt!(int223_handler, 223);
interrupt!(int224_handler, 224);
interrupt!(int225_handler, 225);
interrupt!(int226_handler, 226);
interrupt!(int227_handler, 227);
interrupt!(int228_handler, 228);
interrupt!(int229_handler, 229);
interrupt!(int230_handler, 230);
interrupt!(int231_handler, 231);
interrupt!(int232_handler, 232);
interrupt!(int233_handler, 233);
interrupt!(int234_handler, 234);
interrupt!(int235_handler, 235);
interrupt!(int236_handler, 236);
interrupt!(int237_handler, 237);
interrupt!(int238_handler, 238);
interrupt!(int239_handler, 239);
interrupt!(int240_handler, 240);
interrupt!(int241_handler, 241);
interrupt!(int242_handler, 242);
interrupt!(int243_handler, 243);
interrupt!(int244_handler, 244);
interrupt!(int245_handler, 245);
interrupt!(int246_handler, 246);
interrupt!(int247_handler, 247);
interrupt!(int248_handler, 248);
interrupt!(int249_handler, 249);
interrupt!(int250_handler, 250);
interrupt!(int251_handler, 251);
interrupt!(int252_handler, 252);
interrupt!(int253_handler, 253);
interrupt!(int254_handler, 254);
interrupt!(int255_handler, 255);

fn set_handlers(idt: &mut InterruptDescriptorTable) {
    idt.set_handler(Exception::DivideError.as_idx(), divide_handler, false, None);
    idt.set_handler(Exception::Debug.as_idx(), debug_handler, false, None);
    idt.set_handler(
        Exception::NonMaskableInterrupt.as_idx(),
        nmi_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::Breakpoint.as_idx(),
        breakpoint_handler,
        true,
        None,
    );
    idt.set_handler(
        Exception::OverflowError.as_idx(),
        overflow_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::BoundsRangeExceeded.as_idx(),
        boundsrange_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::InvalidOpcode.as_idx(),
        invalid_opcode_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::DeviceNotAvailable.as_idx(),
        device_not_available_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::DoubleFault.as_idx(),
        double_fault_handler,
        false,
        Some(super::desctables::DOUBLE_FAULT_IST_INDEX.into()),
    );
    idt.set_handler(
        Exception::InvalidTSS.as_idx(),
        invalid_tss_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::SegmentNotPresent.as_idx(),
        segment_not_present_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::StackSegmentFault.as_idx(),
        stack_segment_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::GeneralProtectionFault.as_idx(),
        gpf_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::PageFault.as_idx(),
        pagefault_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::X87FloatingPoint.as_idx(),
        x87_floatingpoint_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::AligmentCheck.as_idx(),
        alignment_check_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::MachineCheck.as_idx(),
        machine_check_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::SIMDFloatingPoint.as_idx(),
        simd_floating_point_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::Virtualization.as_idx(),
        virtualization_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::ControlProtection.as_idx(),
        control_protection_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::HypervisorInjection.as_idx(),
        hypervisor_injection_handler,
        false,
        None,
    );
    idt.set_handler(
        Exception::VMMCommunication.as_idx(),
        vmm_communication_handler,
        false,
        None,
    );
    idt.set_handler(Exception::Security.as_idx(), security_handler, false, None);

    idt.set_handler(32, int32_handler, false, None);
    idt.set_handler(33, int33_handler, false, None);
    idt.set_handler(34, int34_handler, false, None);
    idt.set_handler(35, int35_handler, false, None);
    idt.set_handler(36, int36_handler, false, None);
    idt.set_handler(37, int37_handler, false, None);
    idt.set_handler(38, int38_handler, false, None);
    idt.set_handler(39, int39_handler, false, None);
    idt.set_handler(40, int40_handler, false, None);
    idt.set_handler(41, int41_handler, false, None);
    idt.set_handler(42, int42_handler, false, None);
    idt.set_handler(43, int43_handler, false, None);
    idt.set_handler(44, int44_handler, false, None);
    idt.set_handler(45, int45_handler, false, None);
    idt.set_handler(46, int46_handler, false, None);
    idt.set_handler(47, int47_handler, false, None);
    idt.set_handler(48, int48_handler, false, None);
    idt.set_handler(49, int49_handler, false, None);
    idt.set_handler(50, int50_handler, false, None);
    idt.set_handler(51, int51_handler, false, None);
    idt.set_handler(52, int52_handler, false, None);
    idt.set_handler(53, int53_handler, false, None);
    idt.set_handler(54, int54_handler, false, None);
    idt.set_handler(55, int55_handler, false, None);
    idt.set_handler(56, int56_handler, false, None);
    idt.set_handler(57, int57_handler, false, None);
    idt.set_handler(58, int58_handler, false, None);
    idt.set_handler(59, int59_handler, false, None);
    idt.set_handler(60, int60_handler, false, None);
    idt.set_handler(61, int61_handler, false, None);
    idt.set_handler(62, int62_handler, false, None);
    idt.set_handler(63, int63_handler, false, None);
    idt.set_handler(64, int64_handler, false, None);
    idt.set_handler(65, int65_handler, false, None);
    idt.set_handler(66, int66_handler, false, None);
    idt.set_handler(67, int67_handler, false, None);
    idt.set_handler(68, int68_handler, false, None);
    idt.set_handler(69, int69_handler, false, None);
    idt.set_handler(70, int70_handler, false, None);
    idt.set_handler(71, int71_handler, false, None);
    idt.set_handler(72, int72_handler, false, None);
    idt.set_handler(73, int73_handler, false, None);
    idt.set_handler(74, int74_handler, false, None);
    idt.set_handler(75, int75_handler, false, None);
    idt.set_handler(76, int76_handler, false, None);
    idt.set_handler(77, int77_handler, false, None);
    idt.set_handler(78, int78_handler, false, None);
    idt.set_handler(79, int79_handler, false, None);
    idt.set_handler(80, int80_handler, false, None);
    idt.set_handler(81, int81_handler, false, None);
    idt.set_handler(82, int82_handler, false, None);
    idt.set_handler(83, int83_handler, false, None);
    idt.set_handler(84, int84_handler, false, None);
    idt.set_handler(85, int85_handler, false, None);
    idt.set_handler(86, int86_handler, false, None);
    idt.set_handler(87, int87_handler, false, None);
    idt.set_handler(88, int88_handler, false, None);
    idt.set_handler(89, int89_handler, false, None);
    idt.set_handler(90, int90_handler, false, None);
    idt.set_handler(91, int91_handler, false, None);
    idt.set_handler(92, int92_handler, false, None);
    idt.set_handler(93, int93_handler, false, None);
    idt.set_handler(94, int94_handler, false, None);
    idt.set_handler(95, int95_handler, false, None);
    idt.set_handler(96, int96_handler, false, None);
    idt.set_handler(97, int97_handler, false, None);
    idt.set_handler(98, int98_handler, false, None);
    idt.set_handler(99, int99_handler, false, None);
    idt.set_handler(100, int100_handler, false, None);
    idt.set_handler(101, int101_handler, false, None);
    idt.set_handler(102, int102_handler, false, None);
    idt.set_handler(103, int103_handler, false, None);
    idt.set_handler(104, int104_handler, false, None);
    idt.set_handler(105, int105_handler, false, None);
    idt.set_handler(106, int106_handler, false, None);
    idt.set_handler(107, int107_handler, false, None);
    idt.set_handler(108, int108_handler, false, None);
    idt.set_handler(109, int109_handler, false, None);
    idt.set_handler(110, int110_handler, false, None);
    idt.set_handler(111, int111_handler, false, None);
    idt.set_handler(112, int112_handler, false, None);
    idt.set_handler(113, int113_handler, false, None);
    idt.set_handler(114, int114_handler, false, None);
    idt.set_handler(115, int115_handler, false, None);
    idt.set_handler(116, int116_handler, false, None);
    idt.set_handler(117, int117_handler, false, None);
    idt.set_handler(118, int118_handler, false, None);
    idt.set_handler(119, int119_handler, false, None);
    idt.set_handler(120, int120_handler, false, None);
    idt.set_handler(121, int121_handler, false, None);
    idt.set_handler(122, int122_handler, false, None);
    idt.set_handler(123, int123_handler, false, None);
    idt.set_handler(124, int124_handler, false, None);
    idt.set_handler(125, int125_handler, false, None);
    idt.set_handler(126, int126_handler, false, None);
    idt.set_handler(127, int127_handler, false, None);
    idt.set_handler(128, int128_handler, false, None);
    idt.set_handler(129, int129_handler, false, None);
    idt.set_handler(130, int130_handler, false, None);
    idt.set_handler(131, int131_handler, false, None);
    idt.set_handler(132, int132_handler, false, None);
    idt.set_handler(133, int133_handler, false, None);
    idt.set_handler(134, int134_handler, false, None);
    idt.set_handler(135, int135_handler, false, None);
    idt.set_handler(136, int136_handler, false, None);
    idt.set_handler(137, int137_handler, false, None);
    idt.set_handler(138, int138_handler, false, None);
    idt.set_handler(139, int139_handler, false, None);
    idt.set_handler(140, int140_handler, false, None);
    idt.set_handler(141, int141_handler, false, None);
    idt.set_handler(142, int142_handler, false, None);
    idt.set_handler(143, int143_handler, false, None);
    idt.set_handler(144, int144_handler, false, None);
    idt.set_handler(145, int145_handler, false, None);
    idt.set_handler(146, int146_handler, false, None);
    idt.set_handler(147, int147_handler, false, None);
    idt.set_handler(148, int148_handler, false, None);
    idt.set_handler(149, int149_handler, false, None);
    idt.set_handler(150, int150_handler, false, None);
    idt.set_handler(151, int151_handler, false, None);
    idt.set_handler(152, int152_handler, false, None);
    idt.set_handler(153, int153_handler, false, None);
    idt.set_handler(154, int154_handler, false, None);
    idt.set_handler(155, int155_handler, false, None);
    idt.set_handler(156, int156_handler, false, None);
    idt.set_handler(157, int157_handler, false, None);
    idt.set_handler(158, int158_handler, false, None);
    idt.set_handler(159, int159_handler, false, None);
    idt.set_handler(160, int160_handler, false, None);
    idt.set_handler(161, int161_handler, false, None);
    idt.set_handler(162, int162_handler, false, None);
    idt.set_handler(163, int163_handler, false, None);
    idt.set_handler(164, int164_handler, false, None);
    idt.set_handler(165, int165_handler, false, None);
    idt.set_handler(166, int166_handler, false, None);
    idt.set_handler(167, int167_handler, false, None);
    idt.set_handler(168, int168_handler, false, None);
    idt.set_handler(169, int169_handler, false, None);
    idt.set_handler(170, int170_handler, false, None);
    idt.set_handler(171, int171_handler, false, None);
    idt.set_handler(172, int172_handler, false, None);
    idt.set_handler(173, int173_handler, false, None);
    idt.set_handler(174, int174_handler, false, None);
    idt.set_handler(175, int175_handler, false, None);
    idt.set_handler(176, int176_handler, false, None);
    idt.set_handler(177, int177_handler, false, None);
    idt.set_handler(178, int178_handler, false, None);
    idt.set_handler(179, int179_handler, false, None);
    idt.set_handler(180, int180_handler, false, None);
    idt.set_handler(181, int181_handler, false, None);
    idt.set_handler(182, int182_handler, false, None);
    idt.set_handler(183, int183_handler, false, None);
    idt.set_handler(184, int184_handler, false, None);
    idt.set_handler(185, int185_handler, false, None);
    idt.set_handler(186, int186_handler, false, None);
    idt.set_handler(187, int187_handler, false, None);
    idt.set_handler(188, int188_handler, false, None);
    idt.set_handler(189, int189_handler, false, None);
    idt.set_handler(190, int190_handler, false, None);
    idt.set_handler(191, int191_handler, false, None);
    idt.set_handler(192, int192_handler, false, None);
    idt.set_handler(193, int193_handler, false, None);
    idt.set_handler(194, int194_handler, false, None);
    idt.set_handler(195, int195_handler, false, None);
    idt.set_handler(196, int196_handler, false, None);
    idt.set_handler(197, int197_handler, false, None);
    idt.set_handler(198, int198_handler, false, None);
    idt.set_handler(199, int199_handler, false, None);
    idt.set_handler(200, int200_handler, false, None);
    idt.set_handler(201, int201_handler, false, None);
    idt.set_handler(202, int202_handler, false, None);
    idt.set_handler(203, int203_handler, false, None);
    idt.set_handler(204, int204_handler, false, None);
    idt.set_handler(205, int205_handler, false, None);
    idt.set_handler(206, int206_handler, false, None);
    idt.set_handler(207, int207_handler, false, None);
    idt.set_handler(208, int208_handler, false, None);
    idt.set_handler(209, int209_handler, false, None);
    idt.set_handler(210, int210_handler, false, None);
    idt.set_handler(211, int211_handler, false, None);
    idt.set_handler(212, int212_handler, false, None);
    idt.set_handler(213, int213_handler, false, None);
    idt.set_handler(214, int214_handler, false, None);
    idt.set_handler(215, int215_handler, false, None);
    idt.set_handler(216, int216_handler, false, None);
    idt.set_handler(217, int217_handler, false, None);
    idt.set_handler(218, int218_handler, false, None);
    idt.set_handler(219, int219_handler, false, None);
    idt.set_handler(220, int220_handler, false, None);
    idt.set_handler(221, int221_handler, false, None);
    idt.set_handler(222, int222_handler, false, None);
    idt.set_handler(223, int223_handler, false, None);
    idt.set_handler(224, int224_handler, false, None);
    idt.set_handler(225, int225_handler, false, None);
    idt.set_handler(226, int226_handler, false, None);
    idt.set_handler(227, int227_handler, false, None);
    idt.set_handler(228, int228_handler, false, None);
    idt.set_handler(229, int229_handler, false, None);
    idt.set_handler(230, int230_handler, false, None);
    idt.set_handler(231, int231_handler, false, None);
    idt.set_handler(232, int232_handler, false, None);
    idt.set_handler(233, int233_handler, false, None);
    idt.set_handler(234, int234_handler, false, None);
    idt.set_handler(235, int235_handler, false, None);
    idt.set_handler(236, int236_handler, false, None);
    idt.set_handler(237, int237_handler, false, None);
    idt.set_handler(238, int238_handler, false, None);
    idt.set_handler(239, int239_handler, false, None);
    idt.set_handler(240, int240_handler, false, None);
    idt.set_handler(241, int241_handler, false, None);
    idt.set_handler(242, int242_handler, false, None);
    idt.set_handler(243, int243_handler, false, None);
    idt.set_handler(244, int244_handler, false, None);
    idt.set_handler(245, int245_handler, false, None);
    idt.set_handler(246, int246_handler, false, None);
    idt.set_handler(247, int247_handler, false, None);
    idt.set_handler(248, int248_handler, false, None);
    idt.set_handler(249, int249_handler, false, None);
    idt.set_handler(250, int250_handler, false, None);
    idt.set_handler(251, int251_handler, false, None);
    idt.set_handler(252, int252_handler, false, None);
    idt.set_handler(253, int253_handler, false, None);
    idt.set_handler(254, int254_handler, false, None);
    idt.set_handler(255, int255_handler, false, None);
}

static mut IDT: InterruptDescriptorTable = InterruptDescriptorTable::new();
pub fn init_idt() {
    unsafe {
        set_handlers(&mut IDT);
        IDT.load();
    }
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
