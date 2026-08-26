//! KVM guest support: feature detection, the paravirtual clock (kvmclock), and host panic
//! notification (pvpanic).

use core::sync::atomic::{Ordering, fence};

use twizzler_abi::syscall::{ClockFlags, ClockInfo, FEMTOS_PER_NANO, FemtoSeconds, TimeSpan};
use x86::msr::wrmsr;

use super::memory::phys_to_virt;
use crate::{
    memory::{
        VirtAddr,
        tracker::{FrameAllocFlags, alloc_frame},
    },
    once::Once,
    processor::{
        Processor,
        mp::{all_processors, current_processor},
    },
    time::{ClockHardware, Ticks},
};

const KVM_CPUID_SIGNATURE: u32 = 0x4000_0000;
const KVM_CPUID_FEATURES: u32 = 0x4000_0001;

// Feature bits in KVM_CPUID_FEATURES.EAX.
const KVM_FEATURE_CLOCKSOURCE2: u32 = 1 << 3;
/// The flags field of [PvclockVcpuTimeInfo] is valid, and when it reports stable, readings are
/// consistent across vcpus -- which is what lets every cpu read the one struct the bsp registered.
const KVM_FEATURE_CLOCKSOURCE_STABLE: u32 = 1 << 24;

const MSR_KVM_WALL_CLOCK_NEW: u32 = 0x4b56_4d00;
const MSR_KVM_SYSTEM_TIME_NEW: u32 = 0x4b56_4d01;
const MSR_KVM_SYSTEM_TIME_ENABLE: u64 = 1;

/// KVM's CPUID feature word, or 0 when not running as a KVM guest.
pub fn kvm_features() -> u32 {
    static FEATURES: Once<u32> = Once::new();
    *FEATURES.call_once(|| {
        let sig = unsafe { core::arch::x86_64::__cpuid(KVM_CPUID_SIGNATURE) };
        // "KVMKVMKVM\0\0\0"
        if (sig.ebx, sig.ecx, sig.edx) != (0x4b4d_564b, 0x564b_4d56, 0x4d) {
            return 0;
        }
        unsafe { core::arch::x86_64::__cpuid(KVM_CPUID_FEATURES) }.eax
    })
}

/// Layouts fixed by the KVM pvclock ABI (Linux Documentation/virt/kvm/x86/msr.rst).
#[repr(C)]
#[derive(Clone, Copy)]
struct PvclockWallClock {
    version: u32,
    sec: u32,
    nsec: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PvclockVcpuTimeInfo {
    version: u32,
    pad0: u32,
    tsc_timestamp: u64,
    system_time: u64,
    tsc_to_system_mul: u32,
    tsc_shift: i8,
    flags: u8,
    pad: [u8; 2],
}

const WALL_CLOCK_OFFSET: usize = 0;
const TIME_INFO_OFFSET: usize = 64;

struct Pvclock {
    /// Kernel view of the shared frame the host writes into.
    va: VirtAddr,
    /// Wall-clock time at VM start, ns since the Unix epoch. The host fills this once, at the MSR
    /// write in [init_pvclock]; adding the pvclock system time (ns since VM start) gives now.
    /// A host-side clock step after boot is not seen -- refreshing would take another MSR write.
    wall_base_ns: u64,
}

static PVCLOCK: Once<Pvclock> = Once::new();

impl Pvclock {
    fn time_info(&self) -> *const PvclockVcpuTimeInfo {
        self.va.offset(TIME_INFO_OFFSET).unwrap().as_ptr()
    }

    /// A consistent snapshot of the host-written time parameters. The host brackets updates by
    /// incrementing `version` to odd and back to even, seqlock-style.
    fn read_params(ti: *const PvclockVcpuTimeInfo) -> PvclockVcpuTimeInfo {
        loop {
            let v1 = unsafe { core::ptr::read_volatile(&raw const (*ti).version) };
            if v1 & 1 != 0 {
                core::hint::spin_loop();
                continue;
            }
            let info = unsafe { core::ptr::read_volatile(ti) };
            fence(Ordering::Acquire);
            let v2 = unsafe { core::ptr::read_volatile(&raw const (*ti).version) };
            if v1 == v2 {
                return info;
            }
        }
    }

    /// Nanoseconds since VM start. Reading a stale parameter snapshot is fine -- the rdtsc delta
    /// against it grows to compensate; only a torn one is not, which `read_params` excludes.
    fn system_time_ns(&self) -> u64 {
        let info = Self::read_params(self.time_info());
        let tsc = unsafe { x86::time::rdtsc() };
        let mut delta = tsc.wrapping_sub(info.tsc_timestamp);
        if info.tsc_shift >= 0 {
            delta <<= info.tsc_shift;
        } else {
            delta >>= -info.tsc_shift;
        }
        let ns = ((delta as u128 * info.tsc_to_system_mul as u128) >> 32) as u64;
        info.system_time.wrapping_add(ns)
    }
}

/// Set up the kvmclock shared structures, if this is a KVM guest that offers them. Must run after
/// the frame allocator is up and before [realtime_clock]/[tsc_frequency] can return anything.
pub fn init_pvclock() {
    if kvm_features() & KVM_FEATURE_CLOCKSOURCE2 == 0 {
        return;
    }
    // Leaked: the host writes this frame for the rest of the boot.
    let frame = alloc_frame(FrameAllocFlags::ZEROED);
    let phys = frame.start_address().raw();
    let va = phys_to_virt(frame.start_address());
    unsafe {
        wrmsr(
            MSR_KVM_SYSTEM_TIME_NEW,
            (phys + TIME_INFO_OFFSET as u64) | MSR_KVM_SYSTEM_TIME_ENABLE,
        );
        // The host fills the wall-clock struct synchronously at this write.
        wrmsr(MSR_KVM_WALL_CLOCK_NEW, phys + WALL_CLOCK_OFFSET as u64);
    }
    let wall: *const PvclockWallClock = va.offset(WALL_CLOCK_OFFSET).unwrap().as_ptr();
    let (sec, nsec) = loop {
        let v1 = unsafe { core::ptr::read_volatile(&raw const (*wall).version) };
        if v1 & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let w = unsafe { core::ptr::read_volatile(wall) };
        fence(Ordering::Acquire);
        let v2 = unsafe { core::ptr::read_volatile(&raw const (*wall).version) };
        if v1 == v2 {
            break (w.sec, w.nsec);
        }
    };
    let wall_base_ns = sec as u64 * 1_000_000_000 + nsec as u64;
    logln!(
        "[kernel::arch::kvm] kvmclock enabled (features {:#x}), wall base {}s",
        kvm_features(),
        sec
    );
    PVCLOCK.call_once(|| Pvclock { va, wall_base_ns });
}

/// The TSC frequency in Hz, as derived from the kvmclock scaling parameters -- the host's own
/// answer, with no calibration.
pub fn tsc_frequency() -> Option<u64> {
    let pv = PVCLOCK.poll()?;
    let info = Pvclock::read_params(pv.time_info());
    if info.tsc_to_system_mul == 0 {
        return None;
    }
    let mut khz = (1_000_000u64 << 32) / info.tsc_to_system_mul as u64;
    if info.tsc_shift < 0 {
        khz <<= -info.tsc_shift;
    } else {
        khz >>= info.tsc_shift;
    }
    Some(khz * 1000)
}

/// The kvmclock-backed real-time clock: ns since the Unix epoch. This is the kernel's only wall
/// clock; without it (bare metal, no kvmclock) the "best realtime" slot falls back to holding a
/// monotonic source.
pub struct KvmClock {
    info: ClockInfo,
}

impl ClockHardware for KvmClock {
    fn read(&self) -> Ticks {
        let pv = PVCLOCK.poll().unwrap();
        Ticks {
            value: pv.wall_base_ns.wrapping_add(pv.system_time_ns()),
            rate: FemtoSeconds(FEMTOS_PER_NANO),
        }
    }

    fn info(&self) -> ClockInfo {
        self.info
    }

    fn name(&self) -> &'static str {
        "kvmclock"
    }
}

/// The real-time clock to register, if kvmclock is up and safe to read from every cpu. Only the
/// bsp's time-info struct exists, so this requires the host's stable-clocksource promise; without
/// it a non-bsp rdtsc against the bsp's parameters is not meaningful.
pub fn realtime_clock() -> Option<KvmClock> {
    PVCLOCK.poll()?;
    if kvm_features() & KVM_FEATURE_CLOCKSOURCE_STABLE == 0 {
        logln!("[kernel::arch::kvm] kvmclock present but not stable; not registering wall clock");
        return None;
    }
    Some(KvmClock {
        info: ClockInfo::new(
            TimeSpan::ZERO,
            FemtoSeconds(FEMTOS_PER_NANO),
            FemtoSeconds(FEMTOS_PER_NANO),
            FemtoSeconds(FEMTOS_PER_NANO),
            ClockFlags::empty(),
        ),
    })
}

const KVM_FEATURE_STEAL_TIME: u32 = 1 << 5;
const MSR_KVM_STEAL_TIME: u32 = 0x4b56_4d03;
const MSR_KVM_STEAL_TIME_ENABLE: u64 = 1;

/// Layout fixed by the KVM ABI: 64 bytes, and the registered address must be 64-byte aligned.
#[repr(C)]
struct KvmStealTime {
    /// Cumulative ns this vcpu spent runnable-but-not-running (the host ran something else).
    steal: u64,
    version: u32,
    flags: u32,
    /// Nonzero while the host has this vcpu scheduled out.
    preempted: u8,
    pad0: [u8; 3],
    pad1: [u32; 11],
}
const _: () = assert!(core::mem::size_of::<KvmStealTime>() == 64);

const KVM_VCPU_PREEMPTED: u8 = 1 << 0;

/// Per-cpu steal-time setup, run from `init_secondary` on every cpu. The first caller is the bsp,
/// which runs while the machine is still single-threaded (main.rs, after ACPI cpu enumeration and
/// before `boot_all_secondaries`): it allocates a struct for *every* registered processor, so
/// secondaries do no allocation here -- only the MSR write for their own slot. A whole ZEROED
/// frame per cpu, leaked: alignment comes free and the host writes it for the rest of the boot.
pub fn steal_time_cpu_init() {
    if kvm_features() & KVM_FEATURE_STEAL_TIME == 0 {
        return;
    }
    static ALLOCATED: Once<()> = Once::new();
    ALLOCATED.call_once(|| {
        for p in all_processors().iter().flatten() {
            let frame = alloc_frame(FrameAllocFlags::ZEROED);
            let phys = frame.start_address();
            p.arch.steal_phys.store(phys.raw(), Ordering::Relaxed);
            // Release pairs with the Acquire in the readers: a nonzero va implies the frame
            // behind it is allocated and zeroed.
            p.arch
                .steal_va
                .store(phys_to_virt(phys).raw(), Ordering::Release);
        }
        logln!("[kernel::arch::kvm] steal time enabled");
    });
    let phys = current_processor().arch.steal_phys.load(Ordering::Acquire);
    if phys != 0 {
        unsafe { wrmsr(MSR_KVM_STEAL_TIME, phys | MSR_KVM_STEAL_TIME_ENABLE) };
    }
}

/// Advisory: is this processor's vcpu scheduled out on the host right now? Can go stale the
/// instant it is read, so it is only ever a hint to back off -- never a correctness input. False
/// on bare metal and before steal time is up.
pub fn vcpu_is_preempted(p: &Processor) -> bool {
    let va = p.arch.steal_va.load(Ordering::Acquire);
    if va == 0 {
        return false;
    }
    let st = va as usize as *const KvmStealTime;
    let flags = unsafe { core::ptr::read_volatile(&raw const (*st).preempted) };
    flags & KVM_VCPU_PREEMPTED != 0
}

/// Cumulative ns the host ran something else while this vcpu was runnable. Monotonic; 0 when
/// steal time is off. The one number that says whether a slow measurement was taken on a
/// contended host.
pub fn steal_time_ns(p: &Processor) -> u64 {
    let va = p.arch.steal_va.load(Ordering::Acquire);
    if va == 0 {
        return 0;
    }
    let st = va as usize as *const KvmStealTime;
    loop {
        let v1 = unsafe { core::ptr::read_volatile(&raw const (*st).version) };
        if v1 & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let steal = unsafe { core::ptr::read_volatile(&raw const (*st).steal) };
        fence(Ordering::Acquire);
        let v2 = unsafe { core::ptr::read_volatile(&raw const (*st).version) };
        if v1 == v2 {
            return steal;
        }
    }
}

const KVM_FEATURE_PV_TLB_FLUSH: u32 = 1 << 9;
/// Set *by the guest* in another vcpu's preempted byte, asking KVM to fully flush that vcpu's
/// guest TLB before it next executes an instruction.
const KVM_VCPU_FLUSH_TLB: u8 = 1 << 1;

/// A/B knob for eliding TLB-shootdown IPIs to preempted vcpus (KVM PV TLB flush, feature bit 9).
///
/// A preempted target cannot take the IPI until the host reschedules it, so the sender's wait in
/// [`PendingShootdown::do_wait`] lasts a host scheduling quantum while burning exactly the host
/// cpu the target needs -- the mechanism behind the "TLB shootdown stalled" warnings on a
/// contended host. With this on, such a target is handed to the hypervisor instead and never
/// enters the wait set at all.
///
/// `false` restores the unconditional IPI+wait path; [`try_pv_flush_elide`] then always declines.
pub const PV_TLB_FLUSH: bool = true;

/// Try to hand a shootdown target's invalidation to the hypervisor instead of sending an IPI.
///
/// Returns true only when the byte cmpxchg'd from exactly PREEMPTED to PREEMPTED|FLUSH_TLB. KVM
/// consumes the byte with an xchg at that vcpu's next sched-in and fully flushes its guest TLB
/// (all PCIDs and global entries) before it executes another instruction -- strictly stronger
/// than the queued invalidation it replaces. A successful caller may skip queueing, sending, and
/// *waiting*: the [`DeferredUnmappingOps`]-guarded frame frees stay safe because a descheduled
/// vcpu cannot walk anything, and by the time it can, the flush has happened. Any other byte
/// state -- running, or a flush already pending that the host may be consuming right now --
/// returns false and the caller must send the IPI as before; treating pending-flush as success
/// would race the host's xchg.
pub fn try_pv_flush_elide(p: &Processor) -> bool {
    if !PV_TLB_FLUSH || kvm_features() & KVM_FEATURE_PV_TLB_FLUSH == 0 {
        return false;
    }
    let va = p.arch.steal_va.load(Ordering::Acquire);
    if va == 0 {
        return false;
    }
    let st = va as usize as *mut KvmStealTime;
    let preempted = unsafe { core::sync::atomic::AtomicU8::from_ptr(&raw mut (*st).preempted) };
    // AcqRel: the release half publishes the sender's page-table writes to the host's acquiring
    // xchg at sched-in, so the flushed vcpu walks the new tables.
    preempted
        .compare_exchange(
            KVM_VCPU_PREEMPTED,
            KVM_VCPU_PREEMPTED | KVM_VCPU_FLUSH_TLB,
            Ordering::AcqRel,
            Ordering::Relaxed,
        )
        .is_ok()
}

const PVPANIC_PORT: u16 = 0x505;
const PVPANIC_PANICKED: u8 = 1 << 0;

/// Tell the host the guest has panicked, via qemu's pvpanic ISA device. Under qemu this emits a
/// GUEST_PANICKED event and (with `-action panic=pause`) freezes the machine, so it must come
/// after everything the panic path wants on the console. Gated on the CPUID hypervisor bit rather
/// than on KVM -- the device exists under TCG too, and the port poke is what's avoided on real
/// hardware. cpuid directly, not [kvm_features]: this runs from the panic handler, where a
/// `Once` in mid-initialization must not be re-entered.
pub fn notify_panic() {
    let hypervisor = unsafe { core::arch::x86_64::__cpuid(1) }.ecx & (1 << 31) != 0;
    if hypervisor {
        unsafe { x86::io::outb(PVPANIC_PORT, PVPANIC_PANICKED) };
    }
}
