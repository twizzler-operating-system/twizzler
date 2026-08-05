use alloc::{boxed::Box, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};

use twizzler_abi::arch::XSAVE_LEN;

use super::{
    acpi::get_acpi_root,
    interrupt::InterProcessorInterrupt,
    memory::pagetables::{TlbShootdownInfo, tlb_shootdown_handler},
};
use crate::{
    interrupt::Destination,
    memory::VirtAddr,
    once::Once,
    processor::{Processor, mp::current_processor},
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
            super::syscall::syscall_entry as *const () as usize as u64,
        );
        x86::msr::wrmsr(x86::msr::IA32_STAR, (0x13 << 48) | (0x8 << 32));
        x86::msr::wrmsr(x86::msr::IA32_FMASK, 0xffffffff);
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

    let _has_avx512 = x86::cpuid::CpuId::new()
        .get_extended_feature_info()
        .map(|f| f.has_avx512f())
        .unwrap_or_default();
    let _has_avx2 = x86::cpuid::CpuId::new()
        .get_extended_feature_info()
        .map(|f| f.has_avx2())
        .unwrap_or_default();

    unsafe {
        let mut cr4 = x86::controlregs::cr4()
            | x86::controlregs::Cr4::CR4_ENABLE_SSE
            | x86::controlregs::Cr4::CR4_ENABLE_GLOBAL_PAGES;
        if has_xsave {
            cr4 |= x86::controlregs::Cr4::CR4_ENABLE_OS_XSAVE
                | x86::controlregs::Cr4::CR4_UNMASKED_SSE;
        }
        x86::controlregs::cr4_write(cr4);
        if has_xsave {
            let cpuid = x86::cpuid::CpuId::new();
            let xsave_size = if let Some(ex) = cpuid.get_extended_state_info() {
                let mut xcr0 = x86::controlregs::xcr0();
                xcr0 |= x86::controlregs::Xcr0::XCR0_SSE_STATE
                    | x86::controlregs::Xcr0::XCR0_AVX_STATE
                    | x86::controlregs::Xcr0::XCR0_FPU_MMX_STATE;
                if ex.xcr0_supports_mpx_bndregs() {
                    xcr0 |= x86::controlregs::Xcr0::XCR0_BNDREG_STATE;
                }
                if ex.xcr0_supports_mpx_bndcsr() {
                    xcr0 |= x86::controlregs::Xcr0::XCR0_BNDCSR_STATE;
                }
                if ex.xcr0_supports_avx512_opmask() {
                    xcr0 |= x86::controlregs::Xcr0::XCR0_OPMASK_STATE;
                }
                if ex.xcr0_supports_avx512_zmm_hi256() {
                    xcr0 |= x86::controlregs::Xcr0::XCR0_ZMM_HI256_STATE;
                }
                if ex.xcr0_supports_avx512_zmm_hi16() {
                    xcr0 |= x86::controlregs::Xcr0::XCR0_HI16_ZMM_STATE;
                }
                x86::controlregs::xcr0_write(xcr0);
                ex.xsave_area_size_enabled_features() as usize
            } else {
                1024
            };

            if xsave_size > XSAVE_LEN {
                panic!(
                    "increase xsave length (need {}, have {})",
                    xsave_size, XSAVE_LEN
                );
            }
        }
    }
}

pub unsafe fn init_fpu_state() {
    let mut f: u16 = 0;
    let mut x: u32 = 0;
    unsafe {
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
}

pub fn enumerate_cpus() -> u32 {
    let acpi = get_acpi_root();

    let procinfo = acpi.platform_info().unwrap().processor_info.unwrap();

    let bsp_id = get_bsp_id(Some(&procinfo));

    crate::processor::mp::register(procinfo.boot_processor.local_apic_id, bsp_id);
    for p in procinfo.application_processors.iter() {
        crate::processor::mp::register(p.local_apic_id, bsp_id);
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
        panic!("unsupported CPU: no TSC, which the kernel requires as a clock source");
    }
}

/// Derive this CPU's path through the topology tree, from coarsest grouping to finest. Each
/// entry is the index of the containing node at that level, paired with whether that level
/// groups SMT threads.
pub fn get_topology() -> Vec<(usize, bool)> {
    let cpuid = x86::cpuid::CpuId::new();

    let Some(bitsinfo) = cpuid.get_extended_topology_info() else {
        // No CPUID leaf 0xb. Fall back to the legacy initial APIC ID, which gives each CPU its
        // own node but no grouping information.
        let id = cpuid
            .get_feature_info()
            .map_or(0, |fi| fi.initial_local_apic_id() as usize);
        return alloc::vec![(id, false)];
    };

    // Each level reports a cumulative shift: shifting the x2APIC ID right by shifts[i] yields the
    // ID of the entity one level above level i, so shifts[0] (past the SMT bits) yields the core.
    let mut shifts: Vec<u32> = alloc::vec![];
    let mut smt_level = None;
    let mut id = 0;
    for bi in bitsinfo {
        let level = bi.level_number() as usize;
        shifts.resize(core::cmp::max(level + 1, shifts.len()), 0);
        shifts[level] = bi.shift_right_for_next_apic_id();
        if bi.level_type() == x86::cpuid::TopologyType::SMT && bi.processors() > 1 {
            smt_level = Some(level);
        }
        id = bi.x2apic_id();
    }

    topo_path(&shifts, smt_level, id)
}

/// Build the topology path from the per-level cumulative APIC ID shifts reported by CPUID leaf
/// 0xb. Split out from [get_topology] so it can be tested against topologies we cannot boot.
fn topo_path(shifts: &[u32], smt_level: Option<usize>, id: u32) -> Vec<(usize, bool)> {
    // Walk levels coarsest-first, omitting the outermost (a single package adds no grouping).
    // Levels contributing no ID bits are skipped so we don't nest a node with an identical
    // cpuset; that test is on the shifts rather than the IDs, so every CPU agrees on the shape
    // of the path even though the indices differ.
    let mut path = alloc::vec![];
    for i in (1..shifts.len()).rev() {
        if shifts[i] == shifts[i - 1] {
            continue;
        }
        path.push(((id >> shifts[i - 1]) as usize, false));
    }
    if path.is_empty() {
        path.push((0, false));
    }
    // SMT siblings share one thread node beneath their core.
    if smt_level == Some(0) {
        path.push((0, true));
    }
    path
}

pub struct ArchProcessor {
    wait_word: AtomicU64,
    pub(super) tlb_shootdown_info: TlbShootdownInfo,
    /// The CR3 this processor last switched to. Used by TLB shootdown to skip IPIing
    /// processors whose active address space can't have stale entries for the target
    /// being invalidated. 0 is a sentinel that never matches a real page-table root.
    pub(super) active_cr3: AtomicU64,
}

/// Published in [`ArchProcessor::active_cr3`] while a processor is partway through switching
/// page tables, during which it may hold entries for either the old or the new root. Matches
/// every shootdown target, so such a processor is always conservatively included.
pub(super) const CR3_IN_TRANSITION: u64 = u64::MAX;

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
            active_cr3: AtomicU64::new(0),
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
        // cli/monitor/re-check/mwait is the race-free idle sequence: an interrupt arriving
        // after the has_work() check below stays pending and still breaks us out of mwait,
        // since ECX=1 asks for interrupts as a break event even while masked.
        if mwait_info.break_on_int {
            unsafe { core::arch::asm!("cli") };
        }
        unsafe {
            core::arch::asm!("monitor", "mfence", in("rax") &proc.arch.wait_word, in("rcx") 0, in("rdx") 0);
        }
        if !proc.has_work() {
            unsafe {
                core::arch::asm!("mwait", in("rax") 0, in("rcx") 1);
            }
        }
        // Every path out of here must re-enable interrupts. Whatever broke us out of mwait is
        // still only *pending* while masked, so returning with cli set leaves this processor
        // deaf for the rest of the idle loop -- it never takes the timer, and never runs the
        // TLB shootdown handler, stalling every shootdown that targets it.
        if mwait_info.break_on_int {
            unsafe { core::arch::asm!("sti") };
        }
    } else {
        if proc.has_work() {
            return;
        }
        // sti and hlt must stay adjacent so the interrupt window opens no earlier than the
        // halt; a wakeup landing between them would otherwise be lost. Return with interrupts
        // enabled, for the same reason as above.
        unsafe {
            core::arch::asm!("sti", "hlt");
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

pub fn get_bsp_id(
    maybe_processor_info: Option<&acpi::platform::ProcessorInfo<alloc::alloc::Global>>,
) -> u32 {
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

#[cfg(test)]
mod tests {
    use super::topo_path;

    /// A single CPU reports no meaningful levels, and collapses to one flat node.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_single_cpu() {
        assert_eq!(topo_path(&[0, 0], None, 0), alloc::vec![(0, false)]);
    }

    /// Four cores, no SMT: each core is its own node, no thread level.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_no_smt() {
        for id in 0..4u32 {
            assert_eq!(
                topo_path(&[0, 2], None, id),
                alloc::vec![(id as usize, false)]
            );
        }
    }

    /// Two threads by two cores: siblings share a core node and a thread node.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_smt_two_cores() {
        let paths: alloc::vec::Vec<_> = (0..4u32)
            .map(|id| topo_path(&[1, 2], Some(0), id))
            .collect();
        assert_eq!(paths[0], alloc::vec![(0, false), (0, true)]);
        assert_eq!(paths[0], paths[1]);
        assert_eq!(paths[2], alloc::vec![(1, false), (0, true)]);
        assert_eq!(paths[2], paths[3]);
    }

    /// Two threads by four cores. The old code shifted by (core_bits - logical_bits) == 2 here,
    /// which grouped pairs of cores together instead of SMT siblings.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_smt_four_cores() {
        for id in 0..8u32 {
            assert_eq!(
                topo_path(&[1, 3], Some(0), id),
                alloc::vec![((id >> 1) as usize, false), (0, true)]
            );
        }
        // Siblings share a core; neighbours across a core boundary do not.
        assert_eq!(
            topo_path(&[1, 3], Some(0), 4),
            topo_path(&[1, 3], Some(0), 5)
        );
        assert_ne!(
            topo_path(&[1, 3], Some(0), 5),
            topo_path(&[1, 3], Some(0), 6)
        );
    }

    /// Three levels (SMT, core, die) used to hit the unimplemented!().
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_three_levels() {
        assert_eq!(
            topo_path(&[1, 3, 5], Some(0), 107),
            alloc::vec![(13, false), (53, false), (0, true)]
        );
    }

    /// A level contributing no ID bits is skipped rather than nesting a redundant node.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_degenerate_level_skipped() {
        assert_eq!(
            topo_path(&[1, 2, 2], Some(0), 7),
            alloc::vec![(3, false), (0, true)]
        );
    }

    /// Every CPU must derive the same path length, or the topology tree is inconsistent.
    #[twizzler_kernel_macros::kernel_test]
    fn test_topo_uniform_depth() {
        for shifts in [[0u32, 2, 2], [1, 3, 5], [1, 2, 2], [0, 0, 0]] {
            let len = topo_path(&shifts, Some(0), 0).len();
            for id in 1..64u32 {
                assert_eq!(topo_path(&shifts, Some(0), id).len(), len);
            }
        }
    }
}
