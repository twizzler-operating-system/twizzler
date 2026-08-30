use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use ipi::IpiTask;
use rq::{NR_QUEUES, RunQueue};

use crate::{
    arch::{self, processor::ArchProcessor},
    interrupt,
    once::Once,
    spinlock::Spinlock,
    thread::{Thread, ThreadRef, priority::Priority},
};

pub mod ipi;
pub mod mp;
mod rq;
pub mod sched;
mod timeshare;

#[derive(Debug, Default)]
pub struct ProcessorStats {
    pub preempts: AtomicU64,
    pub wakeups: AtomicU64,
    pub steals: AtomicU64,
    pub idle: AtomicU64,
    pub non_idle: AtomicU64,
    pub hardticks: AtomicU64,
    pub switches: AtomicU64,
    /// Address-space switches (not thread switches -- see `switches`) that reloaded cr3 and
    /// flushed the incoming PCID. Before PCIDs this was the only outcome, so
    /// `noflush / (noflush + flush)` is exactly the fraction of switches the feature changed,
    /// readable from one boot with no baseline run to compare against.
    pub aspace_switch_flush: AtomicU64,
    /// Address-space switches that reloaded cr3 *without* flushing, because this cpu still held a
    /// valid claim on the incoming PCID. Counts only switches that actually wrote cr3: staying on
    /// the context you are already running never flushed, before PCIDs or after, so folding those
    /// in would report a saving that was never there.
    pub aspace_switch_noflush: AtomicU64,
    /// Times another cpu revoked this cpu's right to skip a flush, for an address space this cpu
    /// was not running. The counterweight to the two above -- each one costs at most one future
    /// flush here -- so if this tracks them, invalidation is cancelling the switch saving and
    /// PCIDs are a wash on whatever is running.
    pub aspace_flush_revoked: AtomicU64,
    /// Times this cpu re-asserted its own claim after applying an invalidation that left its
    /// entries for that PCID correct. Every one of these is a revocation above that did *not*
    /// turn into a future flush, so `revoked - reclaimed` is what `aspace_switch_flush` should
    /// now track instead of `revoked`. That the two used to match exactly is what said this was
    /// worth doing.
    pub aspace_claim_reasserted: AtomicU64,
    /// Times this cpu dropped its own claim because an invalidation sent to it did not apply to
    /// the address space it turned out to be running. Rare by construction -- the sender only
    /// targets cpus whose `active_cr3` matched, so this is the window between that read and the
    /// IPI landing -- but it is what keeps [ProcessorStats::aspace_claim_reasserted] honest, so a
    /// reading of zero across a whole boot means the window never opened rather than that it does
    /// not exist.
    pub aspace_claim_dropped: AtomicU64,
    /// Page faults taken on this cpu. Relaxed and lock-free: bumping it under
    /// [`Processor::fault_stats`]'s spinlock cost every fault an interrupt mask and a ticket
    /// acquisition for one monotonic counter. Summed across cpus by the stats read path.
    pub page_faults: AtomicU64,
    /// Shootdown IPIs to this cpu that were elided because its vcpu was preempted and the
    /// invalidation was handed to the hypervisor instead (KVM PV TLB flush). Counted against the
    /// cpu that was spared, like the aspace counters. Zero on bare metal, on a quiet host, and
    /// with the `PV_TLB_FLUSH` knob off -- so a contended validation boot reading zero means the
    /// elision path never ran, not that it is cheap.
    pub tlb_pv_elided: AtomicU64,
}

pub struct Processor {
    pub arch: ArchProcessor,
    rq: RunQueue<NR_QUEUES>,
    current_priority: AtomicU32,
    running: AtomicBool,
    topology_path: Once<Vec<(usize, bool)>>,
    pub id: u32,
    bsp_id: u32,
    pub idle_thread: Once<ThreadRef>,
    pub stats: ProcessorStats,
    ipi_tasks: Spinlock<Vec<Arc<IpiTask>>>,
    exited: Spinlock<Vec<ThreadRef>>,
    /// Deepest this cpu's cleanup list has ever been. See [`Processor::push_exited`].
    exited_max: AtomicUsize,
    is_idle: AtomicBool,
    must_rebalance: AtomicBool,
    /// This cpu's syscall timings and profile. Per-cpu so the kernel-exit path takes no globally
    /// shared lock; summed across cpus on the read path. See [`crate::syscall::SyscallTracking`].
    pub syscall_stats: Spinlock<crate::syscall::SyscallTracking>,
    /// The unconditional syscall counts, outside the lock above; see
    /// [`crate::syscall::SyscallCounts`].
    pub syscall_counts: crate::syscall::SyscallCounts,
    /// This cpu's page-fault stage breakdown, on the same per-cpu terms. See
    /// [`crate::memory::context::virtmem::fault::FaultTracking`].
    pub fault_stats: Spinlock<crate::memory::context::virtmem::fault::FaultTracking>,
    /// This cpu's interrupt counts and timings, on the same per-cpu terms. See
    /// [`crate::interrupt::InterruptTracking`].
    pub interrupt_stats: Spinlock<crate::interrupt::InterruptTracking>,
    /// This cpu's random generator and its batch buffer, on the same per-cpu terms as the stats
    /// above. See [`crate::random`]: `getrandom` used to route every request -- including the
    /// nonce for every object create -- through one global sleeping mutex, holding it across the
    /// whole ChaCha20 generation.
    pub rng: Spinlock<crate::random::PerCpuRng>,
}

impl Processor {
    pub fn new(id: u32, bsp_id: u32) -> Self {
        Self {
            arch: ArchProcessor::default(),
            syscall_stats: Spinlock::new(crate::syscall::SyscallTracking::new()),
            syscall_counts: crate::syscall::SyscallCounts::new(),
            fault_stats: Spinlock::new(crate::memory::context::virtmem::fault::FaultTracking::new()),
            interrupt_stats: Spinlock::new(crate::interrupt::InterruptTracking::new()),
            rng: Spinlock::new(crate::random::PerCpuRng::new()),
            running: AtomicBool::new(false),
            is_idle: AtomicBool::new(false),
            must_rebalance: AtomicBool::new(false),
            rq: RunQueue::new(),
            topology_path: Once::new(),
            id,
            bsp_id,
            idle_thread: Once::new(),
            stats: ProcessorStats::default(),
            ipi_tasks: Spinlock::new(Vec::new()),
            exited: Spinlock::new(Vec::new()),
            exited_max: AtomicUsize::new(0),
            current_priority: AtomicU32::new(0),
        }
    }

    pub fn is_bsp(&self) -> bool {
        self.id == self.bsp_id
    }

    pub fn bsp_id(&self) -> u32 {
        self.bsp_id
    }

    pub fn current_priority(&self) -> Priority {
        let cur = self.current_priority.load(Ordering::Acquire);
        self.rq.current_priority().max(Priority::from_raw(cur))
    }

    pub fn current_load(&self) -> u64 {
        self.rq.current_load()
            + if self.is_idle.load(Ordering::Acquire) {
                0
            } else {
                1
            }
    }

    /// Whether this cpu is currently running its idle thread. Plain atomic read, so it is callable
    /// from contexts that must not take a lock (the mutex wait loop's stall report).
    pub fn is_idle(&self) -> bool {
        self.is_idle.load(Ordering::Acquire)
    }

    /// Lock-free run-queue emptiness, for the same reason as [`Processor::is_idle`].
    pub fn rq_is_empty(&self) -> bool {
        self.rq.is_empty()
    }

    pub fn enter_idle(&self) {
        self.is_idle.store(true, Ordering::Release);
    }

    pub fn exit_idle(&self) {
        self.is_idle.store(false, Ordering::Release);
    }

    pub fn set_rebalance(&self) {
        self.must_rebalance.store(true, Ordering::Release);
    }

    pub fn reset_rebalance(&self) {
        self.must_rebalance.store(false, Ordering::Release);
    }

    pub fn must_rebalance(&self) -> bool {
        self.must_rebalance.load(Ordering::Acquire)
    }

    fn set_topology(&self, topo_path: Vec<(usize, bool)>) {
        self.topology_path.call_once(|| topo_path);
    }

    fn set_running(&self) {
        self.running
            .store(true, core::sync::atomic::Ordering::SeqCst);
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    pub fn set_idle_thread(&self, idle: ThreadRef) {
        self.idle_thread.call_once(|| idle);
    }

    fn enqueue_ipi_task(&self, task: Arc<IpiTask>) {
        task.outstanding.fetch_add(1, Ordering::SeqCst);
        self.ipi_tasks.lock().push(task);
    }

    fn run_ipi_tasks(&self) {
        // Take the list, then run outside the lock. These closures are arbitrary kernel code, and
        // one of them is `schedule_resched` -- which asks whether the current thread needs
        // preempting, and `needs_reschedule` answers no for any thread in a critical section. Run
        // under this lock, every IPI-delivered reschedule request is silently discarded, which is
        // how `Thread::suspend` loses a suspend against a running target.
        let tasks = core::mem::take(&mut *self.ipi_tasks.lock());
        for task in tasks {
            (task.func)();
            task.outstanding.fetch_sub(1, Ordering::Release);
        }
    }

    pub fn push_exited(&self, th: ThreadRef) {
        let len = {
            let mut ex = self.exited.lock();
            ex.push(th);
            ex.len()
        };
        EXITED_BACKLOG.fetch_add(1, Ordering::Relaxed);
        // Per-cpu, because "reaping everywhere is slow" and "reaping stopped on one cpu" produce
        // the same global byte count and want different fixes. A cpu that halts without reaching
        // the reap call again shows a watermark that never comes down; a pacing shortfall shows
        // similar watermarks on every cpu.
        self.exited_max.fetch_max(len, Ordering::Relaxed);
    }

    pub fn cleanup_exited(&self) {
        let item = self.exited.lock().pop();
        if let Some(item) = item {
            EXITED_BACKLOG.fetch_sub(1, Ordering::Relaxed);
            REAPED.fetch_add(1, Ordering::Relaxed);
            let _ = unsafe {
                Box::<ThreadRef, _>::from_raw(*item.self_reference.get().as_ref().unwrap())
            };
        }
    }

    /// Take every entry that has finished switching off its stack.
    ///
    /// A thread pushes itself here from `do_schedule` *before* it switches away, so for a moment it
    /// is on this list while still running on the kernel stack that its drop returns to the free
    /// list. An entry is only safe to drop from another cpu once that switch has saved its stack
    /// pointer, which is what [`Thread::has_left_kernel_stack`] tests. That is what makes a single
    /// reaper thread sound rather than needing one pinned per cpu.
    ///
    /// This guard used to be `is_active_running()`, on the stated belief that the flag went down
    /// after the switch. It goes down *before* `arch_switch_to` instead, and had done for two weeks
    /// when this was written, so the window it was meant to close -- `save_extended_state`, then
    /// `__do_switch`'s seven pushes onto the stack being freed -- was wide open. A cpu draining
    /// inside it dropped the last two refs, `Thread::drop` returned the stack to the free list, and
    /// the next thread created took it, zeroed its top page and wrote its own initial frame over
    /// frames the victim was still using. The victim then jumped through a clobbered slot: a
    /// kernel-mode instruction fetch on a present, non-executable page, panicking in `assert_valid`
    /// with no usable backtrace. 22 occurrences in the 35,761 rounds logged after the reaper
    /// landed, none in the 29,442 before it, first one 28 hours after it.
    ///
    /// Held causally, not just by that correlation: widening the window with a spin between
    /// `set_active_running(false)` and `arch_switch_to`, on builds differing in nothing but the
    /// guard, gave 0/48 rounds passing against 48/48, with `LIVE_STACK_SKIPS` at 102-284 declined
    /// drains per boot on the passing side.
    ///
    /// It does not only wear the fetch panic, which matters when reading a failure against this.
    /// A clobbered frame is a corrupted *slot*, and `__do_switch` restores eight of them: the
    /// return address gives the fetch fault, rflags gives `popfq` of garbage and #GP in
    /// `generic_isr_handler`, and the six callee-saved slots give a wild pointer that surfaces
    /// wherever it is next used. Freeing the `Box<ThreadRef>` also dangles `CURRENT_THREAD`, which
    /// aliases it, so `current_thread_ref()` reads freed heap. The old-guard arm produced all of
    /// it: #GP, "tried to switch to a non-registered sctx", "page fault ... with no memory
    /// context" on a thread reporting `state Running, exiting false`, and a wild kernel data
    /// access -- five sites, none of which occurred in any round with this guard in place.
    pub fn drain_exited(&self, out: &mut Vec<ThreadRef>) {
        let mut taken = 0;
        {
            let mut ex = self.exited.lock();
            let mut i = 0;
            while i < ex.len() {
                if !ex[i].has_left_kernel_stack() {
                    if !ex[i].is_active_running() {
                        LIVE_STACK_SKIPS.fetch_add(1, Ordering::Relaxed);
                    }
                    i += 1;
                    continue;
                }
                out.push(ex.swap_remove(i));
                taken += 1;
            }
        }
        if taken > 0 {
            EXITED_BACKLOG.fetch_sub(taken, Ordering::Relaxed);
        }
    }

    /// Threads on this processor's cleanup list, for the per-cpu diagnostic.
    pub fn exited_len(&self) -> usize {
        self.exited.lock().len()
    }

    /// Deepest this cpu's cleanup list has been since boot.
    pub fn exited_max(&self) -> usize {
        self.exited_max.load(Ordering::Relaxed)
    }

    pub fn maybe_wakeup(&self, th: &Thread) {
        if !self.rq.is_empty() && self.rq.current_priority() > th.effective_priority() {
            interrupt::with_disabled(|| self.wakeup(true));
        }
    }

    pub fn has_work(&self) -> bool {
        !self.rq.is_empty() || self.current_priority.load(Ordering::SeqCst) > 0
    }
}

/// Exited threads waiting for their last reference to be dropped, across all processors.
///
/// Each one holds its whole `Thread` allocation and, through it, a [`KERNEL_STACK_SIZE`] kernel
/// stack that cannot go back on the free list until it is reaped. Neither `nr_threads` nor
/// `nr_pending_exit` can see them: `exit` removes the thread from `ALL_THREADS` before pushing it
/// here.
pub static EXITED_BACKLOG: AtomicUsize = AtomicUsize::new(0);
/// Threads reaped since boot. Read against the backlog: flat while the backlog is non-zero means
/// reaping has stopped, not that it is behind.
pub static REAPED: AtomicUsize = AtomicUsize::new(0);

/// Drains this fix declined that the old `is_active_running()` guard would have allowed -- i.e.
/// times a cpu was about to free a kernel stack its owner was still executing on.
///
/// Kept rather than removed with the bug because it is the only thing that says the guard is doing
/// work: a boot that never enters the window and a boot whose guard has been broken again both
/// report a clean run and nothing else. Nonzero here is the race, observed directly, without
/// needing it to land on something that crashes.
pub static LIVE_STACK_SKIPS: AtomicUsize = AtomicUsize::new(0);

/// Per-cpu cleanup-list depth, now and at its watermark.
///
/// Reported rather than only counted because the aggregate cannot distinguish a pacing shortfall
/// spread over every cpu from one cpu that has stopped reaping entirely.
pub fn report_exited_backlog() {
    let total = EXITED_BACKLOG.load(Ordering::Relaxed);
    let live_stack = LIVE_STACK_SKIPS.load(Ordering::Relaxed);
    if total == 0 && live_stack == 0 {
        return;
    }
    let mut line = alloc::string::String::new();
    for (i, p) in mp::all_processors().iter().enumerate() {
        if let Some(p) = p {
            if p.is_running() {
                use core::fmt::Write;
                let _ = write!(line, " cpu{}={}/{}", i, p.exited_len(), p.exited_max());
            }
        }
    }
    logln!(
        "[reap] backlog={} reaped={} livestack={}{}",
        total,
        REAPED.load(Ordering::Relaxed),
        live_stack,
        line
    );
}

/// Set once every cpu has installed its thread pointer, after which no cpu can be running with TLS
/// unset and the per-cpu check below is unnecessary.
static ALL_TLS_READY: AtomicBool = AtomicBool::new(false);

/// Called when the last secondary has come up. `boot_all_secondaries` waits for each processor's
/// `running` flag, which `secondary_entry` sets only after `arch::processor::init` has installed
/// its thread pointer, so by the time it returns the claim holds for every cpu.
pub fn note_all_tls_ready() {
    ALL_TLS_READY.store(true, Ordering::Release);
}

/// Whether this cpu can safely touch thread-local storage.
///
/// The per-cpu answer costs an `rdmsr` (the thread pointer lives in `IA32_FS_BASE` and there is
/// nowhere cheaper to read it from before TLS exists), and `current_thread_ref` -- one of the
/// hottest functions in the kernel -- calls this on every invocation. Once every cpu has a thread
/// pointer the answer is permanently yes for all of them, so a plain global load replaces the MSR
/// read for the whole life of the system; the MSR path survives only for boot.
#[inline]
pub fn tls_ready() -> bool {
    if core::intrinsics::likely(ALL_TLS_READY.load(Ordering::Relaxed)) {
        return true;
    }
    crate::arch::processor::tls_ready()
}

pub const KERNEL_STACK_SIZE: usize = 2 * 1024 * 1024; // 2M

/// Spin waits while a condition (cond) is true, regularly running architecture-dependent spin-wait
/// code along with the provided pause function. The cond function should not mutate state, and it
/// should be fast (ideally reading a single, perhaps atomic, memory value + a comparison). The
/// pause function, on the other hand, can be heavier-weight, and may do arbitrary work (within
/// the context of the caller). The cond function will be called some multiple of times between
/// calls to pause, and if cond returns false, then this function immediately returns. The
/// [core::hint::spin_loop] function is called between calls to cond.
pub fn spin_wait_until<R>(mut until: impl FnMut() -> Option<R>, mut pause: impl FnMut()) -> R {
    const NR_SPIN_LOOPS: usize = 100;
    loop {
        for _ in 0..NR_SPIN_LOOPS {
            if let Some(ret) = until() {
                return ret;
            }
            core::hint::spin_loop();
        }
        arch::processor::spin_wait_iteration();
        pause();
    }
}
