use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{
    acpi::get_acpi_root,
    interrupt::InterProcessorInterrupt,
    memory::pagetables::{tlb_shootdown_handler, TlbShootdownInfo},
};
use crate::{
    interrupt::Destination,
    memory::VirtAddr,
    once::Once,
    processor::{current_processor, Processor},
};

#[repr(C)]
struct GsScratch {
    kernel_stack: u64,
    kernel_fs: u64,
    scratch: u64,
}

impl GsScratch {
    const fn new() -> Self {
        Self {
            kernel_fs: 0,
            kernel_stack: 0,
            scratch: 0,
        }
    }
}

pub fn init(tls: VirtAddr) {
    unsafe {
        let efer = x86::msr::rdmsr(x86::msr::IA32_EFER);
        x86::msr::wrmsr(x86::msr::IA32_EFER, efer | 1);
    };

    unsafe {
        let mut misc = x86::msr::rdmsr(x86::msr::IA32_MISC_ENABLE);
        misc |= 1 << 18;
        x86::msr::wrmsr(x86::msr::IA32_MISC_ENABLE, misc);
    }
    unsafe {
        x86::msr::wrmsr(
            x86::msr::IA32_LSTAR,
            super::syscall::syscall_entry as usize as u64,
        );
        x86::msr::wrmsr(x86::msr::IA32_STAR, (0x10 << 48) | (0x8 << 32));
        x86::msr::wrmsr(x86::msr::IA32_FMASK, 0xffffffffffffffff);
    }
    /* unsafe {
        x86_64::registers::segmentation::FS::set_reg(SegmentSelector::new(
            0,
            x86_64::PrivilegeLevel::Ring0,
        ))
    };*/
    let cpuid = x86::cpuid::CpuId::new().get_extended_feature_info();
    let mut gs_scratch = Box::new(GsScratch::new());
    gs_scratch.kernel_fs = tls.raw();
    // Intentionally leak this memory, we don't need to reference it again outside interrupt
    // assembly code.
    let gs_scratch = Box::into_raw(gs_scratch);
    if let Some(ef) = cpuid {
        if ef.has_fsgsbase() {
            unsafe {
                let cr4 = x86::controlregs::cr4();
                x86::controlregs::cr4_write(cr4 | x86::controlregs::Cr4::CR4_ENABLE_FSGSBASE);
            }
        }
    }
    let has_xsave = x86::cpuid::CpuId::new()
        .get_feature_info()
        .map(|f| f.has_xsave())
        .unwrap_or_default();
    unsafe { x86::msr::wrmsr(x86::msr::IA32_FS_BASE, tls.raw()) };
    unsafe { x86::msr::wrmsr(x86::msr::IA32_GS_BASE, gs_scratch as u64) };
    unsafe { x86::msr::wrmsr(x86::msr::IA32_KERNEL_GSBASE, 0) };

    unsafe {
        let mut cr4 = x86::controlregs::cr4() | x86::controlregs::Cr4::CR4_ENABLE_SSE;
        if has_xsave {
            cr4 |= x86::controlregs::Cr4::CR4_ENABLE_OS_XSAVE
                | x86::controlregs::Cr4::CR4_UNMASKED_SSE;
        }
        x86::controlregs::cr4_write(cr4);
        if has_xsave {
            let xcr0 = x86::controlregs::xcr0();
            x86::controlregs::xcr0_write(
                xcr0 | x86::controlregs::Xcr0::XCR0_SSE_STATE
                    | x86::controlregs::Xcr0::XCR0_AVX_STATE
                    | x86::controlregs::Xcr0::XCR0_FPU_MMX_STATE,
            );
        }
    }
}

pub unsafe fn init_fpu_state() {
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

pub fn enumerate_cpus() -> u32 {
    let acpi = get_acpi_root();

    let procinfo = acpi.platform_info().unwrap().processor_info.unwrap();

    let bsp_id = get_bsp_id(Some(&procinfo));

    crate::processor::register(procinfo.boot_processor.local_apic_id, bsp_id);
    for p in procinfo.application_processors {
        crate::processor::register(p.local_apic_id, bsp_id);
    }

    bsp_id
}

/// Determine what hardware clock sources are available
/// on the processor and register them in the time subsystem.
pub fn enumerate_clocks() {
    // for now we only use the TSC
    // in the future we will explore using other time sources

    let cpuid = x86::cpuid::CpuId::new();

    // check if processor has TSC
    let has_tsc = cpuid
        .get_feature_info()
        .map_or(false, |finfo| finfo.has_tsc());
    if has_tsc {
        // saves reference to tsc clock source into global array
        crate::time::register_clock(super::tsc::Tsc::new());
    } else {
        todo!("running on processor that does not have a TSC");
    }
}

pub fn get_topology() -> Vec<(usize, bool)> {
    let cpuid = x86::cpuid::CpuId::new();
    let bitsinfo = cpuid
        .get_extended_topology_info()
        .expect("TODO: implement support for deriving topology without this feature");
    let mut levels = alloc::vec![];
    let mut smt_level = None;
    let mut id = 0;
    for bi in bitsinfo {
        levels.resize(
            core::cmp::max(bi.level_number() as usize + 1, levels.len()),
            0,
        );
        levels[bi.level_number() as usize] = bi.shift_right_for_next_apic_id();
        if bi.level_type() == x86::cpuid::TopologyType::SMT && bi.processors() > 1 {
            smt_level = Some(bi.level_number());
        }
        id = bi.x2apic_id(); //TODO: is this okay to use?
    }
    if levels.len() != 2 {
        unimplemented!("more extensible topo information");
    }
    let lowest_is_smt = smt_level.is_some() && smt_level.unwrap() == 0;
    let logical_bits = levels[0];
    let core_bits = levels[1];
    let core_id = id >> (core_bits - logical_bits);
    //let thread_id = id & ((1 << logical_bits) - 1);
    if logical_bits == 0 {
        alloc::vec![(core_id as usize, lowest_is_smt)]
    } else if logical_bits == core_bits {
        alloc::vec![(0, false), (0, lowest_is_smt)]
    } else {
        alloc::vec![(core_id as usize, false), (0, lowest_is_smt)]
    }
}

pub struct ArchProcessor {
    wait_word: AtomicU64,
    pub(super) tlb_shootdown_info: TlbShootdownInfo,
}

impl core::fmt::Debug for ArchProcessor {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ArchProcessor")
            .field("wait_word", &self.wait_word)
            .finish()
    }
}

impl Default for ArchProcessor {
    fn default() -> Self {
        Self {
            wait_word: Default::default(),
            tlb_shootdown_info: TlbShootdownInfo::new(),
        }
    }
}

#[derive(Default, Debug)]
pub struct MwaitInfo {
    break_on_int: bool,
}

static HAS_MWAIT: Once<Option<MwaitInfo>> = Once::new();

fn has_mwait() -> &'static Option<MwaitInfo> {
    HAS_MWAIT.call_once(|| {
        let cpuid = x86::cpuid::CpuId::new();
        let features = cpuid.get_feature_info();
        let info = if features.unwrap().has_monitor_mwait() {
            let mut info = MwaitInfo::default();
            let mwait_features = cpuid.get_monitor_mwait_info();
            if let Some(mwait_features) = mwait_features {
                if mwait_features.supported_c1_states() > 0 {
                    info.break_on_int = mwait_features.interrupts_as_break_event();
                    Some(info)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
        info
    })
}

pub fn halt_and_wait() {
    /* TODO: spin a bit */
    /* TODO: parse cstates and actually put the cpu into deeper and deeper sleep */
    let proc = current_processor();
    let mwait_info = has_mwait();
    if let Some(mwait_info) = mwait_info {
        {
            if mwait_info.break_on_int {
                unsafe { core::arch::asm!("cli") };
            }
            {
                let sched = proc.schedlock();
                unsafe {
                    core::arch::asm!("monitor", "mfence", in("rax") &proc.arch.wait_word, in("rcx") 0, in("rdx") 0);
                }
                if sched.has_work() {
                    return;
                }
            }
            unsafe {
                core::arch::asm!("mwait", in("rax") 0, in("rcx") 1);
            }
        }
    } else {
        {
            let sched = proc.schedlock();
            if sched.has_work() {
                return;
            }
        }
        unsafe {
            core::arch::asm!("sti", "hlt", "cli");
        }
    }
}

impl Processor {
    pub fn wakeup(&self, signal: bool) {
        if has_mwait().is_some() {
            self.arch.wait_word.store(1, Ordering::SeqCst);
            if !signal {
                return;
            }
        }
        crate::interrupt::send_ipi(
            Destination::Single(self.id),
            InterProcessorInterrupt::Reschedule,
        );
    }
}

pub fn tls_ready() -> bool {
    unsafe { x86::msr::rdmsr(x86::msr::IA32_FS_BASE) != 0 }
    //unsafe { x86::bits64::segmentation::rdfsbase() != 0 }
}

pub fn get_bsp_id(maybe_processor_info: Option<&acpi::platform::ProcessorInfo>) -> u32 {
    match maybe_processor_info {
        None => {
            let acpi = get_acpi_root();
            let processor_info = acpi.platform_info().unwrap().processor_info.unwrap();

            processor_info.boot_processor.local_apic_id
        }
        Some(p) => p.boot_processor.local_apic_id,
    }
}

pub fn spin_wait_iteration() {
    tlb_shootdown_handler();
}
