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
    spinlock::Spinlock,
    thread::{current_memory_context, current_thread_ref, locktrack},
    time::TimeStatCollector,
    trace::mgr::TRACE_MGR,
};

/// DIAG (Mode B): the last few (slot, object) pairs removed from any context. A
/// `MemoryContextViolation` says only that a slot has no region; what we need to know is whether
/// it *had* one, and which object, which is otherwise unrecoverable once the region is gone.
const UNMAP_HIST_LEN: usize = 32;
const UNMAP_NOTE_LEN: usize = 48;

#[derive(Clone, Copy)]
struct UnmapRecord {
    slot: usize,
    id: ObjID,
    note: [u8; UNMAP_NOTE_LEN],
    note_len: usize,
}

static UNMAP_HIST: Spinlock<([UnmapRecord; UNMAP_HIST_LEN], usize)> = Spinlock::new((
    [UnmapRecord {
        slot: 0,
        id: ObjID::new(0),
        note: [0; UNMAP_NOTE_LEN],
        note_len: 0,
    }; UNMAP_HIST_LEN],
    0,
));

/// Whether to keep the unmap history. It costs a global spinlock and a note summarize on *every*
/// unmap, which is on the measured path of the map/unmap benches; it only pays for itself while
/// chasing a `MemoryContextViolation`.
pub const UNMAP_HISTORY: bool = false;

pub(super) fn note_unmap(slot: usize, obj: &crate::obj::ObjectRef) {
    if !UNMAP_HISTORY {
        return;
    }
    let mut note = [0u8; UNMAP_NOTE_LEN];
    let note_len = obj.get_notes().summarize(&mut note);
    let mut hist = UNMAP_HIST.lock();
    let idx = hist.1 % UNMAP_HIST_LEN;
    hist.0[idx] = UnmapRecord {
        slot,
        id: obj.id(),
        note,
        note_len,
    };
    hist.1 += 1;
}

fn report_unmap_history(slot: usize) {
    let hist = UNMAP_HIST.lock();
    let total = hist.1;
    for i in 0..UNMAP_HIST_LEN.min(total) {
        let rec = hist.0[(total - 1 - i) % UNMAP_HIST_LEN];
        if rec.slot == slot {
            log::error!(
                "fault-diag: slot {:x} last held object {} ({}), unmapped {} unmaps ago",
                rec.slot,
                rec.id,
                core::str::from_utf8(&rec.note[..rec.note_len]).unwrap_or("<non-utf8 note>"),
                i
            );
            return;
        }
    }
    log::error!(
        "fault-diag: slot {:x} not in the last {} unmaps",
        slot,
        UNMAP_HIST_LEN.min(total)
    );
}

/// Collect a per-stage breakdown of the fault path, per cpu, and dump it at `debug_shutdown`.
///
/// Same shape and same reasoning as [`crate::syscall::SYSCALL_PROFILE`]: off by default, because
/// it times a dozen spans per fault and takes a (per-cpu, so uncontended) lock for each one, and
/// the numbers it reports include that cost.
pub const FAULT_PROFILE: bool = false;

/// A/B: resolve the faulting region through the per-thread slot memo, rather than by taking the
/// context-wide `regions` mutex on every fault.
///
/// Exists because the memo's cost was first measured against a baseline built from a different
/// tree state, so its uncontended cost and its contended saving were both confounded by unrelated
/// changes. Flipping this const is the only difference between the two arms.
/// Off: the memo existed to route around the context-wide `regions` mutex, which the `SlotMgr`
/// refactor removed. A/B at `memoab-on`/`memoab-off` (3 sequential-lane rounds each, no overlap
/// between arms): uncontended soft fault 996 -> 936 ns mean, contended flat (7314 -> 7238),
/// thread_sync_ping_pong 2150 -> 1822 ns -- disabling it wins or ties everywhere measured. The
/// `SlotMemo` machinery stays for `lookup_object_ref_batch` until that path gets its own A/B.
pub const FAULT_SLOT_MEMO: bool = false;

/// Stages of one fault, in the order they run. `Total` is the whole of [`page_fault`], so it also
/// captures whatever is not attributed to a stage.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum FaultStage {
    /// `log_fault` + `assert_valid` + `check_violations` + `get_context`.
    Prologue = 0,
    /// Looking up the faulting address's region, i.e. the `regions` lock plus a `MapRegion` clone.
    Region,
    /// `check_security`, which includes a *second* region lookup for the executing object.
    Security,
    /// All of `MapRegion::handle_fault`, which the next four break down.
    Handle,
    /// Taking the object's page-table lock.
    PtLock,
    /// `ensure_in_core`: the fill, including any pager round trip.
    EnsureCore,
    /// Inside `ensure_in_core`: acquiring the frames for the fill (precharge, plus any wait for
    /// memory and the lock re-acquisition that a wait costs).
    Precharge,
    /// Inside `ensure_in_core`: installing those frames in the object's page table.
    Fill,
    Cow,
    /// `ensure_object_mapped`: installing the object-table entry in the address space.
    MapObject,
    Total,
}
pub const NR_STAGES: usize = FaultStage::Total as usize + 1;

/// [`FaultStage`] names, in variant order.
pub const STAGE_NAMES: [&str; NR_STAGES] = [
    "prologue",
    "region",
    "security",
    "handle",
    "pt_lock",
    "ensure_core",
    "precharge",
    "fill",
    "cow",
    "map_object",
    "TOTAL",
];

/// What kind of fault it was, counted alongside the stages.
#[derive(Clone, Copy)]
#[repr(usize)]
pub enum FaultClass {
    User = 0,
    Kernel,
    /// The page was already present: a permission or object-table fault, not a fill.
    Present,
    Read,
    Write,
    Exec,
    /// Reached the pager.
    Pager,
    /// Copied on write.
    Cow,
    /// Installed an object-table entry rather than finding one already there.
    Mapped,
}
const NR_CLASSES: usize = FaultClass::Mapped as usize + 1;

/// Bucket bounds for [`FaultTracking::buckets`], in nanoseconds; the last bucket is everything
/// above. A mutex here is a *sleeping* lock, so an acquisition either costs a few hundred ns or
/// blocks for as long as the holder takes -- means alone cannot tell those apart, and every stage
/// in this path has a max in the milliseconds.
const BUCKET_NS: [u64; 3] = [1_000, 10_000, 100_000];
const NR_BUCKETS: usize = BUCKET_NS.len() + 1;

pub struct FaultTracking {
    /// Duration of every fault, which `SysInfo` reports once [`TIMING_ON`] is latched. The
    /// unconditional fault *count* lives in `ProcessorStats::page_faults`, outside this lock.
    time: TimeStatCollector,
    stages: [TimeStatCollector; NR_STAGES],
    buckets: [[usize; NR_BUCKETS]; NR_STAGES],
    classes: [usize; NR_CLASSES],
}

impl FaultTracking {
    pub fn new() -> Self {
        Self {
            time: TimeStatCollector::new(),
            stages: core::array::from_fn(|_| TimeStatCollector::new()),
            buckets: [[0; NR_BUCKETS]; NR_STAGES],
            classes: [0; NR_CLASSES],
        }
    }
}

/// Read the clock for a stage timing, but only when [`FAULT_PROFILE`] will use the answer.
///
/// `Instant::now()` is not free -- an `Arc<dyn ClockHardware>` dispatch, an `rdtsc`, and a u128
/// multiply-and-divide -- and the fault path takes a dozen of these. `FAULT_PROFILE` is a const,
/// so with it off both this and the matching [`record_stage`] fold away to nothing.
#[inline(always)]
pub fn stage_start() -> Instant {
    if FAULT_PROFILE {
        Instant::now()
    } else {
        Instant::zero()
    }
}

/// Time one stage of the fault path. Compiles away entirely when [`FAULT_PROFILE`] is off.
pub fn record_stage(stage: FaultStage, start: Instant) {
    if !FAULT_PROFILE {
        return;
    }
    let dur: twizzler_abi::syscall::TimeSpan = (Instant::now() - start).into();
    let ns = dur.as_nanos() as u64;
    let bucket = BUCKET_NS
        .iter()
        .position(|b| ns < *b)
        .unwrap_or(NR_BUCKETS - 1);
    crate::interrupt::with_disabled(|| {
        let mut stats = crate::processor::mp::current_processor().fault_stats.lock();
        stats.stages[stage as usize].add_sample(dur);
        stats.buckets[stage as usize][bucket] += 1;
    });
}

pub fn record_class(class: FaultClass) {
    if !FAULT_PROFILE {
        return;
    }
    crate::interrupt::with_disabled(|| {
        crate::processor::mp::current_processor()
            .fault_stats
            .lock()
            .classes[class as usize] += 1;
    });
}

/// Per-stage (count, total nanoseconds) summed over cpus, for [`crate::perfmark`] to difference.
///
/// Cumulative, never reset: the marker subtracts consecutive snapshots, which is the only reading
/// that survives other cpus faulting between two marks.
pub fn stage_snapshot() -> [(usize, u64); NR_STAGES] {
    let mut out = [(0usize, 0u64); NR_STAGES];
    if !FAULT_PROFILE {
        return out;
    }
    crate::processor::mp::with_each_active_processor(|p| {
        let stats = p.fault_stats.lock();
        for (i, stage) in stats.stages.iter().enumerate() {
            out[i].0 += stage.count();
            out[i].1 += (stage.sum_femtos() / 1_000_000) as u64;
        }
    });
    out
}

pub fn print_fault_profile() {
    if !FAULT_PROFILE {
        return;
    }
    let mut stages: [TimeStatCollector; NR_STAGES] =
        core::array::from_fn(|_| TimeStatCollector::new());
    let mut buckets = [[0usize; NR_BUCKETS]; NR_STAGES];
    let mut classes = [0usize; NR_CLASSES];
    crate::processor::mp::with_each_active_processor(|p| {
        let stats = p.fault_stats.lock();
        for (i, stage) in stats.stages.iter().enumerate() {
            stages[i].merge(stage);
            for (b, count) in stats.buckets[i].iter().enumerate() {
                buckets[i][b] += count;
            }
        }
        for (i, count) in stats.classes.iter().enumerate() {
            classes[i] += count;
        }
    });

    logln!("== fault profile ==");
    for (i, name) in STAGE_NAMES.iter().enumerate() {
        let stat = stages[i].get_stats();
        let count = stages[i].count();
        if count == 0 {
            continue;
        }
        logln!(
            "  {:>11}: {:>6} x {:>7} ns = {:>7} us  min {:>6} max {:>9}  [<1us {:>6} <10us {:>5} <100us {:>4} >= {:>4}]",
            name,
            count,
            stat.mean.as_nanos(),
            (stat.mean.as_nanos() as usize * count) / 1000,
            stat.min.as_nanos(),
            stat.max.as_nanos(),
            buckets[i][0],
            buckets[i][1],
            buckets[i][2],
            buckets[i][3],
        );
    }
    const CLASS_NAMES: [&str; NR_CLASSES] = [
        "user", "kernel", "present", "read", "write", "exec", "pager", "cow", "mapped",
    ];
    for (i, name) in CLASS_NAMES.iter().enumerate() {
        logln!("  {:>11}: {}", name, classes[i]);
    }
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
/// same reasoning as [`crate::syscall::SYSCALL_PROFILE`]'s `TIMING_ON`: one relaxed load of a
/// static written approximately never, so it sits shared and clean in every cpu's L1.
static TIMING_ON: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(FAULT_PROFILE);

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
                // Names the object that last occupied this slot, which is the thing a violation
                // cannot report once the region is gone -- and every category-A wedge so far has
                // been on a thread the pager reported a deleted object to.
                if let Ok(slot) = TryInto::<Slot>::try_into(addr) {
                    report_unmap_history(slot.raw());
                }
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
    let t = stage_start();
    let perms = check_security(
        sctx_id,
        id.clone(),
        addr,
        cause,
        ip,
        exec_info,
        info.default_prot,
    );
    record_stage(FaultStage::Security, t);
    let perms = perms?;

    // Do we need to switch contexts?
    if perms.ctx != sctx_id {
        current_thread_ref().map(|ct| ct.switch_sctx(perms.ctx));
    }

    let t = stage_start();
    let res = info.handle_fault(
        addr, ip, cause, flags, start_time, perms, perms.ctx, map_ctx,
    );
    record_stage(FaultStage::Handle, t);
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

    // Through the per-thread slot memo rather than the context-wide `regions` mutex. The single
    // acquisition this replaces was already the cheap arrangement -- it exists because these used
    // to be two -- but it is still one lock every fault in the compartment must pass through, and
    // under concurrent faults it convoys: 155 ns to 7.5 us per fault at smp4, 58% of the contended
    // increase. A hit here costs an uncontended per-thread spinlock and an `Arc` clone; a miss
    // costs that plus the acquisition it would have taken anyway.
    let (mut region, exec_region) = if FAULT_SLOT_MEMO {
        ctx.lookup_fault_regions(slot, exec_slot)
    } else {
        (
            ctx.regions.lookup_region(slot),
            exec_slot.and_then(|s| ctx.regions.lookup_region(s)),
        )
    };
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
    let t = stage_start();
    log_fault(addr, cause, flags, ip);
    assert_valid(addr, cause, flags, ip);
    check_violations(addr, cause, flags, ip)?;

    let (ctx, sctx_id, map_ctx) = get_context(addr, flags);
    record_stage(FaultStage::Prologue, t);
    if FAULT_PROFILE {
        record_class(if flags.contains(PageFaultFlags::USER) {
            FaultClass::User
        } else {
            FaultClass::Kernel
        });
        if flags.contains(PageFaultFlags::PRESENT) {
            record_class(FaultClass::Present);
        }
        record_class(match cause {
            MemoryAccessKind::Read => FaultClass::Read,
            MemoryAccessKind::Write => FaultClass::Write,
            MemoryAccessKind::InstructionFetch => FaultClass::Exec,
        });
    }

    let t = stage_start();
    let info = get_map_region(addr, &ctx, cause, ip, needs_exec_info(ip, sctx_id));
    record_stage(FaultStage::Region, t);
    let (info, exec_info) = info?;
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
    record_stage(FaultStage::Total, start_time);
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
        if let UpcallInfo::MemoryContextViolation(_) = upcall
            && let Ok(slot) = TryInto::<Slot>::try_into(addr)
        {
            report_unmap_history(slot.raw());
        }
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
