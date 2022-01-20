use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use alloc::{boxed::Box, vec::Vec};
use x86_64::{
    instructions::segmentation::Segment64,
    registers::{control::Cr4Flags, model_specific::EferFlags},
    VirtAddr,
};

use crate::{
    interrupt::Destination,
    processor::{current_processor, Processor},
};

use super::{acpi::get_acpi_root, interrupt::InterProcessorInterrupt};

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
        x86_64::registers::control::Efer::update(|f| f.insert(EferFlags::SYSTEM_CALL_EXTENSIONS))
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
    gs_scratch.kernel_fs = tls.as_u64();
    let gs_scratch = Box::into_raw(gs_scratch);
    if let Some(ef) = cpuid {
        if ef.has_fsgsbase() {
            unsafe { x86_64::registers::control::Cr4::update(|f| f.insert(Cr4Flags::FSGSBASE)) };
            unsafe {
                x86_64::registers::segmentation::GS::write_base(VirtAddr::new(gs_scratch as u64))
            };
            unsafe { x86_64::registers::segmentation::FS::write_base(tls) };
        } else {
            /* we use these instruction in interrupt handling */
            panic!("no support for rdfsbase and wrfsbase");
        }
    } else {
        panic!("no support for rdfsbase and wrfsbase");
    }
    unsafe { x86::msr::wrmsr(x86::msr::IA32_FS_BASE, tls.as_u64()) };
    unsafe { x86::msr::wrmsr(x86::msr::IA32_GS_BASE, gs_scratch as u64) };
    unsafe { x86::msr::wrmsr(x86::msr::IA32_KERNEL_GSBASE, 0) };
}

pub fn enumerate_cpus() {
    let acpi = get_acpi_root();

    let procinfo = acpi.platform_info().unwrap().processor_info.unwrap();
    crate::processor::register(
        procinfo.boot_processor.local_apic_id,
        !procinfo.boot_processor.is_ap,
    );
    for p in procinfo.application_processors {
        crate::processor::register(p.local_apic_id, !p.is_ap);
    }
}

pub fn get_topology() -> Vec<(usize, bool)> {
    let cpuid = x86::cpuid::CpuId::new();
    let vendor = cpuid.get_vendor_info().unwrap();
    if vendor.as_str() != "GenuineIntel" {
        unimplemented!("AMD support for topology determination");
    }
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

#[derive(Default, Debug)]
pub struct ArchProcessor {
    wait_word: AtomicU64,
}

static HAS_MWAIT: AtomicU32 = AtomicU32::new(0);

fn has_mwait() -> bool {
    let state = HAS_MWAIT.load(Ordering::SeqCst);
    if state == 0 {
        let cpuid = x86::cpuid::CpuId::new();
        let features = cpuid.get_feature_info();
        if !features.unwrap().has_monitor_mwait() {
            HAS_MWAIT.store(1, Ordering::SeqCst);
            return has_mwait();
        }
        let mwait_features = cpuid.get_monitor_mwait_info();
        if let Some(_mwait_features) = mwait_features {
            HAS_MWAIT.store(2, Ordering::SeqCst);
            return has_mwait();
        }
        HAS_MWAIT.store(1, Ordering::SeqCst);
        return has_mwait();
    }
    state > 1
}

pub fn halt_and_wait() {
    /* TODO: spin a bit */
    /* TODO: parse cstates and actually put the cpu into deeper and deeper sleep */
    let proc = current_processor();
    if has_mwait() {
        {
            {
                let sched = proc.sched.lock();
                unsafe {
                    asm!("monitor", "mfence", in("rax") &proc.arch.wait_word, in("rcx") 0, in("rdx") 0);
                }
                if sched.has_work() {
                    return;
                }
            }
            unsafe {
                asm!("mwait", in("rax") 0, in("rcx") 1);
            }
        }
    } else {
        {
            let sched = proc.sched.lock();
            if sched.has_work() {
                return;
            }
        }
        unsafe {
            asm!("sti", "hlt", "cli");
        }
    }
}

impl Processor {
    pub fn wakeup(&self, signal: bool) {
        if has_mwait() {
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
    unsafe { x86::bits64::segmentation::rdfsbase() != 0 }
}
