use alloc::sync::Arc;

use twizzler_abi::{
    object::{MAX_SIZE, ObjID, Protections},
    trace::{CONTEXT_FAULT, TraceKind},
    upcall::{
        MemoryAccessKind, MemoryContextViolationInfo, ObjectMemoryFaultInfo, SecurityViolationInfo,
        UpcallInfo,
    },
};
use twizzler_rt_abi::error::ObjectError;

use super::{PageFaultFlags, Slot, region::MapRegion};
use crate::{
    arch::VirtAddr,
    instant::Instant,
    memory::context::{ContextRef, kernel_context},
    obj::PageNumber,
    security::{AccessInfo, KERNEL_SCTX, PermsInfo},
    thread::{current_memory_context, current_thread_ref, locktrack},
    time::TimeStatCollector,
    trace::mgr::TRACE_MGR,
};

pub struct FaultTracking {
    /// Duration of every fault, which `SysInfo` reports once [`TIMING_ON`] is latched. The
    /// unconditional fault *count* lives in `ProcessorStats::page_faults`, outside this lock.
    time: TimeStatCollector,
}

impl FaultTracking {
    pub fn new() -> Self {
        Self {
            time: TimeStatCollector::new(),
        }
    }
}

/// Per-object page-fault census: which objects the faults are going to, and what kind.
///
/// One hashed slot lookup and a pair of relaxed `fetch_add`s per fault, so it can stay on for a
/// whole `cargo build`.
///
/// One call per fault, at the end of `MapRegion::handle_fault`, where every classifier the fault
/// produced is already known -- rather than at the sites that produce them, which would cost one
/// table lookup each.
pub mod census {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    use twizzler_abi::object::ObjID;

    /// Armed by `--kernel-arg=--diag=fault`; see [`crate::kdiag_fault`].
    pub fn enabled() -> bool {
        crate::kdiag_fault()
    }

    /// Distinct objects tracked.
    ///
    /// Slots are claimed first-come and never released, so this is not a top-N: at 512 a guest
    /// `cargo build` put 159,859 of 175,695 faults into `OVERFLOW`, and the slots that *were* held
    /// belonged to whatever faulted during boot rather than to the workload. The per-kind totals
    /// below are what survive that -- they are global and cannot overflow -- so the class split is
    /// always complete even when the per-object listing is not.
    const CAP: usize = 4096;

    #[derive(Clone, Copy)]
    #[repr(usize)]
    pub enum Kind {
        /// Absent page filled from nothing: no pager, no COW. The zero-fill path.
        Fill = 0,
        /// The fill reached the pager.
        Pager,
        /// Copied on write.
        Cow,
        /// Nothing was filled; the fault installed the object-table entry into the address space.
        MapOnly,
        /// Nothing was filled and nothing was installed -- a permission fault, or a refault.
        Present,
    }
    pub const NR_KINDS: usize = Kind::Present as usize + 1;
    pub const KIND_NAMES: [&str; NR_KINDS] = ["fill", "pager", "cow", "map", "present"];

    /// Slot key: the low half of the object id, claimed by CAS. `HI` is stored after the claim and
    /// is always written with the same value by the same object, so a reader cannot observe a
    /// mismatched pair -- only, briefly, a zero high half.
    static LO: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];
    static HI: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];
    static N: [[AtomicU64; NR_KINDS]; CAP] =
        [const { [const { AtomicU64::new(0) }; NR_KINDS] }; CAP];
    static OVERFLOW: AtomicU64 = AtomicU64::new(0);
    static TOTAL: AtomicU64 = AtomicU64::new(0);
    /// Per-kind totals over every fault, whether or not its object found a slot.
    static KIND_TOTAL: [AtomicU64; NR_KINDS] = [const { AtomicU64::new(0) }; NR_KINDS];

    /// Protection class of the faulting region: executable (code/libraries), read-only (rodata),
    /// or read-write (heap/stack/bss/data). Global and overflow-proof, unlike the per-object table
    /// -- this is what tells file-backed code faults (shareable across compartments) apart from
    /// anon read-write churn without needing a per-object note. `EXEC` wins over `WRITE`.
    pub const NR_PROT: usize = 3;
    pub const PROT_NAMES: [&str; NR_PROT] = ["exec", "ro", "rw"];
    static PROT_KIND: [[AtomicU64; NR_KINDS]; NR_PROT] =
        [const { [const { AtomicU64::new(0) }; NR_KINDS] }; NR_PROT];

    /// Object-note snapshot (userspace's role label: `heap:<sctx>`, `mk:<file>:<line>`, ...),
    /// captured here because the object is usually deleted before the census prints. Notes are
    /// added by userspace *after* create -- often after the first fault -- so a claimed slot
    /// whose note is still empty retries on later faults (one relaxed byte-read per fault).
    pub const NOTE_LEN: usize = 24;
    static NOTES: [[core::sync::atomic::AtomicU8; NOTE_LEN]; CAP] =
        [const { [const { core::sync::atomic::AtomicU8::new(0) }; NOTE_LEN] }; CAP];

    /// Fault instruction pointers, so the census can name the CODE that touches new memory and
    /// not only the object it landed in.
    ///
    /// Sized generously but not paranoid: distinct fault SITES are few (a handful of hot touch
    /// loops -- memset, memcpy, first-touch), unlike the distinct OBJECTS the table above keys on,
    /// which is why that one overflowed 159,859 of 175,695 faults on a guest build. `IP_OVERFLOW`
    /// reports saturation either way rather than leaving it silent.
    const IP_CAP: usize = 8192;
    static IP_KEY: [AtomicU64; IP_CAP] = [const { AtomicU64::new(0) }; IP_CAP];
    static IP_N: [[AtomicU64; NR_KINDS]; IP_CAP] =
        [const { [const { AtomicU64::new(0) }; NR_KINDS] }; IP_CAP];
    static IP_OVERFLOW: AtomicU64 = AtomicU64::new(0);

    /// Record the userspace instruction that took the fault. Costs two atomics and touches no user
    /// memory -- the ip is already in the trap frame, so unlike a stack walk this cannot take a
    /// nested fault.
    pub fn record_ip(ip: u64, kind: Kind) {
        if ip == 0 {
            return;
        }
        let mut idx = ((ip.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as usize) % IP_CAP;
        for _ in 0..64 {
            let cur = IP_KEY[idx].load(Relaxed);
            if cur == ip {
                IP_N[idx][kind as usize].fetch_add(1, Relaxed);
                return;
            }
            if cur == 0 {
                if IP_KEY[idx]
                    .compare_exchange(0, ip, core::sync::atomic::Ordering::AcqRel, Relaxed)
                    .is_ok()
                {
                    IP_N[idx][kind as usize].fetch_add(1, Relaxed);
                    return;
                }
                continue; // lost the race for this slot; re-read it
            }
            idx = (idx + 1) % IP_CAP;
        }
        IP_OVERFLOW.fetch_add(1, Relaxed);
    }

    /// Top fault sites by instruction pointer. Selection rather than a full sort: this runs on the
    /// console path, and console writes are slow enough to distort what follows them.
    pub fn print_ips() {
        const TOP: usize = 40;
        let tot = |i: usize| IP_N[i].iter().map(|c| c.load(Relaxed)).sum::<u64>();
        let mut best = [(0usize, 0u64); TOP];
        for i in 0..IP_CAP {
            if IP_KEY[i].load(Relaxed) == 0 {
                continue;
            }
            let t = tot(i);
            if t <= best[TOP - 1].1 {
                continue;
            }
            let mut j = TOP - 1;
            while j > 0 && best[j - 1].1 < t {
                best[j] = best[j - 1];
                j -= 1;
            }
            best[j] = (i, t);
        }
        let distinct = (0..IP_CAP)
            .filter(|i| IP_KEY[*i].load(Relaxed) != 0)
            .count();
        logln!(
            "== fault sites by ip: {} distinct, {} faults with no slot ==",
            distinct,
            IP_OVERFLOW.load(Relaxed)
        );
        logln!("  {:>18}  {:>9}  {}", "ip", "total", KIND_NAMES.join(" "));
        for (i, t) in best.iter() {
            if *t == 0 {
                break;
            }
            logln!(
                "  FAULTIP {:#018x} {:9} {} {} {} {} {}",
                IP_KEY[*i].load(Relaxed),
                t,
                IP_N[*i][0].load(Relaxed),
                IP_N[*i][1].load(Relaxed),
                IP_N[*i][2].load(Relaxed),
                IP_N[*i][3].load(Relaxed),
                IP_N[*i][4].load(Relaxed),
            );
        }
    }

    pub fn record(id: ObjID, kind: Kind, prot: usize, note: impl FnOnce(&mut [u8]) -> usize) {
        TOTAL.fetch_add(1, Relaxed);
        KIND_TOTAL[kind as usize].fetch_add(1, Relaxed);
        PROT_KIND[prot.min(NR_PROT - 1)][kind as usize].fetch_add(1, Relaxed);
        let raw = id.raw();
        let lo = raw as u64;
        let hi = (raw >> 64) as u64;
        // An id of 0 cannot be distinguished from an unclaimed slot; nothing faults against it.
        if lo == 0 {
            OVERFLOW.fetch_add(1, Relaxed);
            return;
        }
        let home = ((lo ^ hi) as usize >> 4) % CAP;
        let mut i = home;
        for _ in 0..CAP {
            match LO[i].compare_exchange(0, lo, Relaxed, Relaxed) {
                Ok(_) => {
                    HI[i].store(hi, Relaxed);
                    break;
                }
                Err(cur) if cur == lo && HI[i].load(Relaxed) == hi => break,
                Err(_) => i = (i + 1) % CAP,
            }
        }
        if LO[i].load(Relaxed) != lo {
            OVERFLOW.fetch_add(1, Relaxed);
            return;
        }
        N[i][kind as usize].fetch_add(1, Relaxed);
        if NOTES[i][0].load(Relaxed) == 0 {
            let mut buf = [0u8; NOTE_LEN];
            let n = note(&mut buf).min(NOTE_LEN);
            // First byte last, since a nonzero first byte is what marks the slot captured.
            for j in (0..n).rev() {
                NOTES[i][j].store(buf[j], Relaxed);
            }
        }
    }

    /// Sorted by total, descending -- an unsorted dump reads as a ranking to anyone who truncates
    /// it, which is how a flat counter once got reported as a hundredfold change.
    pub fn print() {
        if TOTAL.load(Relaxed) == 0 {
            return;
        }
        let total_of = |i: usize| N[i].iter().map(|c| c.load(Relaxed)).sum::<u64>();
        let mut order: [u16; CAP] = core::array::from_fn(|i| i as u16);
        for a in 0..CAP {
            for b in (a + 1)..CAP {
                if total_of(order[b] as usize) > total_of(order[a] as usize) {
                    order.swap(a, b);
                }
            }
        }
        logln!(
            "== fault census: {} faults, {} in objects that found no slot ==",
            TOTAL.load(Relaxed),
            OVERFLOW.load(Relaxed),
        );
        for (i, name) in KIND_NAMES.iter().enumerate() {
            logln!("  {:>8}: {}", name, KIND_TOTAL[i].load(Relaxed));
        }
        logln!("  by protection x kind ({}):", KIND_NAMES.join(" "));
        for (p, pname) in PROT_NAMES.iter().enumerate() {
            let row: [u64; NR_KINDS] = core::array::from_fn(|k| PROT_KIND[p][k].load(Relaxed));
            logln!(
                "    {:>5}: {} {} {} {} {}  (sum {})",
                pname,
                row[0],
                row[1],
                row[2],
                row[3],
                row[4],
                row.iter().sum::<u64>(),
            );
        }
        logln!(
            "  {:>10}  {:>34}  {}",
            "TOTAL",
            "OBJECT",
            KIND_NAMES.join(" ")
        );
        for &i in order.iter() {
            let i = i as usize;
            let lo = LO[i].load(Relaxed);
            if lo == 0 {
                continue;
            }
            let total = total_of(i);
            if total == 0 {
                continue;
            }
            let id = ObjID::new(((HI[i].load(Relaxed) as u128) << 64) | lo as u128);
            let mut note = [0u8; NOTE_LEN];
            for (j, b) in note.iter_mut().enumerate() {
                *b = NOTES[i][j].load(Relaxed);
            }
            let end = note.iter().position(|b| *b == 0).unwrap_or(NOTE_LEN);
            logln!(
                "  {:>10}  {:>34}  {} {} {} {} {}  {}",
                total,
                id,
                N[i][0].load(Relaxed),
                N[i][1].load(Relaxed),
                N[i][2].load(Relaxed),
                N[i][3].load(Relaxed),
                N[i][4].load(Relaxed),
                core::str::from_utf8(&note[..end]).unwrap_or("?"),
            );
        }
        print_ips();
    }
}

pub fn print_fault_profile() {
    census::print();
}

pub fn fill_stats(stats: &mut twizzler_abi::syscall::MemoryStats) {
    // Asking for the stats is what turns their collection on; see `TIMING_ON`.
    TIMING_ON.store(true, core::sync::atomic::Ordering::Relaxed);
    let mut time = TimeStatCollector::new();
    crate::processor::mp::with_each_active_processor(|p| {
        stats.page_fault_count += p
            .stats
            .page_faults
            .load(core::sync::atomic::Ordering::Relaxed) as usize;
        time.merge(&p.fault_stats.lock().time);
    });
    stats.page_fault_stats = time.get_stats();
}

/// Whether the per-fault duration is measured at all.
///
/// Counting is unconditional and cheap; the two `Instant::now()` calls that bracket the fault are
/// not, and until someone reads `MemoryStats` nothing looks at what they produce. Same shape and
/// same reasoning as the syscall path's `TIMING_ON`: one relaxed load of a static written
/// approximately never, so it sits shared and clean in every cpu's L1.
static TIMING_ON: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Consecutive identical faults before the loop is called a loop. Two in a row happen normally (two
/// threads on one page, a COW retry); a fault that returns `Ok` without mapping anything reaches
/// this in milliseconds.
const REFAULT_LOOP_AT: u32 = 1000;

/// Report budget for the above, so a livelocked thread cannot flood the console and move the very
/// window being investigated.
static REFAULT_LOOP: locktrack::diag::Counter =
    locktrack::diag::Counter::new("same address faulted in a loop");

#[allow(unused_variables)]
fn log_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    if let Some(ct) = current_thread_ref() {
        // Relaxed: these are one thread's own record of its own previous fault, read only by the
        // refault detector below and by diagnostics. Nothing orders anything against them, and
        // SeqCst here is three locked exchanges on every fault in the system.
        let old_addr = ct
            .last_pf_addr
            .swap(addr.raw(), core::sync::atomic::Ordering::Relaxed);
        let old_flags = ct
            .last_pf_flags
            .swap(flags.bits(), core::sync::atomic::Ordering::Relaxed);
        let old_kind = ct
            .last_pf_kind
            .swap(cause as u32, core::sync::atomic::Ordering::Relaxed);
        if old_addr == addr.raw() && old_flags == flags.bits() && old_kind == cause as u32 {
            // Counted, not just noticed. Comparing against only the previous fault cannot tell a
            // benign repeat from a livelock, and `log::debug!` is filtered out at the level these
            // runs use -- so this detector has been present and silent.
            let n = ct
                .last_pf_count
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            if n >= REFAULT_LOOP_AT && n.is_power_of_two() && REFAULT_LOOP.hit() {
                emerglogln!(
                    "refault loop: thread {} faulted {} times at {:?} ({:?}, {:?}) ip={:?}",
                    ct.id(),
                    n,
                    addr,
                    cause,
                    flags,
                    ip,
                );
            }
        } else {
            ct.last_pf_count
                .store(0, core::sync::atomic::Ordering::Relaxed);
        }
    }
}

fn assert_valid(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    if flags.contains(PageFaultFlags::INVALID) {
        panic!("page table contains invalid bits for address {:?}", addr);
    }
    if !flags.contains(PageFaultFlags::USER) && cause == MemoryAccessKind::InstructionFetch {
        logln!(
            "==> {} {} {}",
            addr.is_kernel_object_memory(),
            addr.is_kernel(),
            ip.is_kernel()
        );
        panic!(
            "kernel page-fault at IP {:?} caused by {:?} to/from {:?} with flags {:?}",
            ip, cause, addr, flags
        );
    }
    if !flags.contains(PageFaultFlags::USER) && addr.is_kernel() && !addr.is_kernel_object_memory()
    {
        panic!(
            "kernel page-fault at IP {:?} caused by {:?} to/from {:?} with flags {:?}",
            ip, cause, addr, flags
        );
    }
}

fn check_violations(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    _ip: VirtAddr,
) -> Result<(), UpcallInfo> {
    if flags.contains(PageFaultFlags::USER) && addr.is_kernel() {
        // info!("generating upcall, addr: {addr:?}, flags: {flags:?}");
        return Err(UpcallInfo::MemoryContextViolation(
            MemoryContextViolationInfo::new(addr.raw(), cause),
        ));
    }
    Ok(())
}

/// Resolve the contexts a fault runs against: the one whose regions name the faulting address, the
/// active security context, and the one a resulting mapping is installed in.
///
/// The last is the calling thread's own context, and it is *not* always the first -- a fault on
/// kernel object memory looks the region up in the kernel context but installs into whatever
/// context the faulting thread is running under. `MapRegion::handle_fault` used to resolve it
/// again for itself, which is a second `current_thread_ref` and a second `Arc` clone per fault of
/// a value this function already had in hand and threw away.
fn get_context(addr: VirtAddr, flags: PageFaultFlags) -> (ContextRef, ObjID, ContextRef) {
    let sctx_id = current_thread_ref()
        .map(|ct| ct.active_sctx_id())
        .unwrap_or(KERNEL_SCTX);
    let user_ctx = current_memory_context();
    let map_ctx = user_ctx.clone().unwrap_or_else(|| kernel_context().clone());
    if addr.is_kernel_object_memory() {
        assert!(!flags.contains(PageFaultFlags::USER));
        (kernel_context().clone(), KERNEL_SCTX, map_ctx)
    } else {
        // Was "seen once and never reproduced". It reproduces: twice in 48 rounds of a build with
        // `drain_exited`'s old `is_active_running()` guard plus a widened exit window, and zero
        // times in 110 rounds with the current guard. Both hits reported `state Some(Running),
        // exiting Some(false), critical Some(false)` -- the "plain running user thread" this
        // comment called unexplained.
        //
        // The explanation is that the thread was freed under us. `set_current_thread` does
        // `ptr.write(*r)` where `r` derives from `thread.self_reference` (thread.rs:286-288) --
        // a bitwise duplicate of the `ThreadRef`, taking no reference count of its own. The
        // owning copy lives in the Box that `drain_exited` reclaims via `Box::from_raw` on that
        // same `self_reference` (processor.rs:238). So dropping the Box can release the last
        // count while `CURRENT_THREAD` still points at the allocation; a premature drain makes
        // `current_thread_ref()` a reference into freed heap, and
        // `current_memory_context()` then reads a dead field and yields `None`. No stack clobber
        // is needed, which is why this presents differently from the instruction-fetch panic.
        //
        // Read the fields below with that in mind: they are fetched *through the dangling
        // reference*, so on this path they describe freed memory rather than a thread. "Running,
        // not exiting, not critical" is what a stale allocation happens to say -- it is the
        // signature of the bug, not a report of the thread's state. The distinctions the next
        // paragraph draws are still the right ones to draw for any *other* cause.
        //
        // A thread mid-exit or mid-context-switch has a reason to have dropped its context; a
        // plain running user thread does not, and that is a different bug from a stray kernel
        // access to a non-kernel-object address.
        match user_ctx {
            Some(ctx) => (ctx, sctx_id, map_ctx),
            None => {
                let ct = current_thread_ref();
                panic!(
                    "page fault at {:?} (flags {:?}) with no memory context: thread {:?} ({:?}), \
                     state {:?}, exiting {:?}, critical {:?}, sctx {}",
                    addr,
                    flags,
                    ct.as_ref().map(|t| t.id()),
                    ct.as_ref().map(|t| t.objid()),
                    ct.as_ref().map(|t| t.get_state()),
                    ct.as_ref().map(|t| t.is_exiting()),
                    ct.as_ref().map(|t| t.is_critical()),
                    sctx_id,
                );
            }
        }
    }
}

fn check_object_addr(
    page_number: PageNumber,
    id: ObjID,
    cause: MemoryAccessKind,
    addr: VirtAddr,
) -> Result<(), UpcallInfo> {
    if page_number.is_zero() || page_number.as_byte_offset() >= MAX_SIZE {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            ObjectError::NotMapped.into(),
            cause,
            addr.into(),
        )));
    }
    Ok(())
}

fn check_security(
    user_sctx: ObjID,
    id: ObjID,
    addr: VirtAddr,
    cause: MemoryAccessKind,
    ip: VirtAddr,
    exec_info: Option<ExecInfo>,
    default_prot: Protections,
) -> Result<PermsInfo, UpcallInfo> {
    if ip.is_kernel() || user_sctx.raw() == 0 {
        return Ok(PermsInfo {
            ctx: user_sctx,
            provide: Protections::all(),
            restrict: Protections::empty(),
        });
    }
    // `needs_exec_info` above agrees with the condition just tested, so this is present whenever
    // the path gets here.
    let exec_info = exec_info.ok_or(UpcallInfo::MemoryContextViolation(
        MemoryContextViolationInfo::new(ip.raw(), MemoryAccessKind::InstructionFetch),
    ))?;
    let access_kind = match cause {
        MemoryAccessKind::Read => Protections::READ,
        MemoryAccessKind::Write => Protections::WRITE | Protections::READ,
        MemoryAccessKind::InstructionFetch => Protections::EXEC | Protections::READ,
    };
    let access_info = AccessInfo {
        target_id: id,
        access_kind,
        exec_id: Some(exec_info.id),
        exec_off: ip - exec_info.base,
    };
    if let Some(ct) = current_thread_ref() {
        let perms = ct.check_active_access(&access_info, default_prot);

        if perms.provide & !perms.restrict & access_kind == access_kind {
            return Ok(perms);
        }
        let perms = ct.search_access(&access_info, default_prot);
        if perms.provide & !perms.restrict & access_kind != access_kind {
            log::error!(
                "security violation: addr={:?}, cause={:?}, ip={:?}, perms={:?}, access_info={:?}",
                addr,
                cause,
                ip,
                perms,
                access_info
            );
            Err(UpcallInfo::SecurityViolation(SecurityViolationInfo {
                address: addr.raw(),
                access_kind: cause,
            }))
        } else {
            Ok(perms)
        }
    } else {
        Ok(PermsInfo {
            ctx: KERNEL_SCTX,
            provide: Protections::all(),
            restrict: Protections::empty(),
        })
    }
}

fn page_fault_to_region(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    ip: VirtAddr,
    sctx_id: ObjID,
    info: Arc<MapRegion>,
    exec_info: Option<ExecInfo>,
    map_ctx: ContextRef,
) -> Result<(), UpcallInfo> {
    // Only for `trace_fault`, which is off unless a sink is listening -- so ask first rather than
    // reading the clock on every fault to hand it a number nobody looks at.
    let start_time = if TRACE_MGR.any_enabled(TraceKind::Context, CONTEXT_FAULT) {
        Instant::now()
    } else {
        Instant::zero()
    };
    let id = info.object.id();
    let page_number = PageNumber::from_address(addr);

    // Step 1: Check for address validity and check for security violations.
    check_object_addr(page_number, id, cause, addr)?;

    // `check_id` used to run here, per fault, to recover the object's default protections. It is
    // memoized in a `Once` that `insert_object` has already filled, so the region carries the
    // answer instead. TODO: enforce the id check itself.
    let perms = check_security(
        sctx_id,
        id.clone(),
        addr,
        cause,
        ip,
        exec_info,
        info.default_prot,
    )?;

    // Do we need to switch contexts?
    if perms.ctx != sctx_id {
        current_thread_ref().map(|ct| ct.switch_sctx(perms.ctx));
    }

    let res = info.handle_fault(
        addr, ip, cause, flags, start_time, perms, perms.ctx, map_ctx,
    );
    if let Err(e) = res {
        return Err(UpcallInfo::ObjectMemoryFault(ObjectMemoryFaultInfo::new(
            id,
            e,
            cause,
            addr.into(),
        )));
    }
    Ok(())
}

/// What [`check_security`] needs about the object the faulting thread is executing in: enough to
/// name the access, and no more. Deliberately not a `MapRegion`: cloning one is four `Arc` bumps
/// and four matching drops, for two fields.
#[derive(Clone, Copy)]
struct ExecInfo {
    id: ObjID,
    base: VirtAddr,
}

/// Whether the fault needs [`ExecInfo`]. Agrees with `check_security`'s early return, which is what
/// lets the lookup below be skipped rather than performed and discarded.
fn needs_exec_info(ip: VirtAddr, sctx_id: ObjID) -> bool {
    !ip.is_kernel() && sctx_id.raw() != 0
}

/// Look up the faulting address's region and, in the *same* acquisition of the regions lock, the
/// object executing at `ip`.
///
/// These used to be two separate calls -- one here and one from `check_security` -- so every fault
/// that reached a security check took the regions mutex twice and cloned two `MapRegion`s. The
/// lock is 750 ns and a lookup-plus-clone is another 650, on a path whose whole floor is ~6 us.
fn get_map_region(
    addr: VirtAddr,
    ctx: &ContextRef,
    cause: MemoryAccessKind,
    ip: VirtAddr,
    want_exec: bool,
) -> Result<(Arc<MapRegion>, Option<ExecInfo>), UpcallInfo> {
    let violation = |addr: VirtAddr, cause| {
        UpcallInfo::MemoryContextViolation(MemoryContextViolationInfo::new(addr.raw(), cause))
    };
    let slot: Slot = addr.try_into().map_err(|_| violation(addr, cause))?;
    let exec_slot = match want_exec {
        true => Some(
            TryInto::<Slot>::try_into(ip)
                .map_err(|_| violation(ip, MemoryAccessKind::InstructionFetch))?,
        ),
        false => None,
    };
    let exec_of = |region: &Arc<MapRegion>| ExecInfo {
        id: region.object.id(),
        base: region.range.start,
    };

    let mut region = ctx.regions.lookup_region(slot);
    let exec_region = exec_slot.and_then(|s| ctx.regions.lookup_region(s));
    let mut exec = exec_region.as_ref().map(&exec_of);

    // Whatever this context did not have may still be a kernel object.
    if region.is_none() || (exec_slot.is_some() && exec.is_none()) {
        let kctx = kernel_context();
        if region.is_none() {
            region = kctx.regions.lookup_region(slot);
        }
        if exec.is_none() {
            exec = exec_slot.and_then(|s| kctx.regions.lookup_region(s).as_ref().map(&exec_of));
        }
    }

    let region = region.ok_or(violation(addr, cause))?;
    if exec_slot.is_some() && exec.is_none() {
        return Err(violation(ip, MemoryAccessKind::InstructionFetch));
    }
    Ok((region, exec))
}

pub fn do_page_fault(
    addr: VirtAddr,
    cause: MemoryAccessKind,
    flags: PageFaultFlags,
    ip: VirtAddr,
) -> Result<(), UpcallInfo> {
    log_fault(addr, cause, flags, ip);
    assert_valid(addr, cause, flags, ip);
    check_violations(addr, cause, flags, ip)?;

    let (ctx, sctx_id, map_ctx) = get_context(addr, flags);
    let (info, exec_info) = get_map_region(addr, &ctx, cause, ip, needs_exec_info(ip, sctx_id))?;
    page_fault_to_region(addr, cause, flags, ip, sctx_id, info, exec_info, map_ctx)
}

pub fn page_fault(addr: VirtAddr, cause: MemoryAccessKind, flags: PageFaultFlags, ip: VirtAddr) {
    let timing = TIMING_ON.load(core::sync::atomic::Ordering::Relaxed);
    let start_time = if timing {
        Instant::now()
    } else {
        Instant::zero()
    };
    let res = do_page_fault(addr, cause, flags, ip);
    // Per-cpu, for the same reason the syscall counters are (see `SyscallCounts`): this used to
    // be one global spinlock taken on every fault, then a per-cpu one -- an interrupt mask and a
    // ticket acquisition per fault, for one monotonic counter. Relaxed and lock-free now; a
    // preemption between resolving the processor and the increment lands the count on the cpu the
    // thread just left, which the summing read path (`fill_stats`) does not care about. A fault
    // before this cpu's tls is up goes uncounted; those are early-boot kernel faults, and the
    // alternative is a null check on the hot path.
    if crate::processor::tls_ready() {
        let cp = crate::processor::mp::current_processor();
        cp.stats
            .page_faults
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // Same count, charged to the thread as well as the cpu: the per-cpu one answers "where
        // is the system faulting", this one answers "who is faulting", and neither can be
        // derived from the other.
        if let Some(thread) = crate::thread::current_thread_ref() {
            thread
                .stats
                .faults
                .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        if timing {
            let sample = (Instant::now() - start_time).into();
            crate::interrupt::with_disabled(|| {
                cp.fault_stats.lock().time.add_sample(sample);
            });
        }
    }
    if flags.contains(PageFaultFlags::USER) && !ip.is_kernel() && !addr.is_kernel() {
        log::trace!(
            "done page-fault: {:?} {:?} {:?} ip={:?}",
            addr,
            cause,
            flags,
            ip
        );
    }
    if let Err(upcall) = res {
        if !flags.contains(PageFaultFlags::USER) {
            // The upcall is queued onto the thread's user entry frame, so it does nothing for a
            // fault taken in the kernel: this handler returns to the faulting kernel instruction,
            // which faults again. There is no unwind path, so name the culprit loudly — the bound
            // in `send_upcall` is what actually stops it.
            log::error!(
                "kernel-mode fault at ip {:?} on unresolvable address {:?} ({:?}) cannot be unwound",
                ip,
                addr,
                cause
            );
        }
        current_thread_ref().unwrap().send_upcall(upcall);
    }
}
