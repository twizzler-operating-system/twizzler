use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

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
    is_idle: AtomicBool,
    must_rebalance: AtomicBool,
}

impl Processor {
    pub fn new(id: u32, bsp_id: u32) -> Self {
        Self {
            arch: ArchProcessor::default(),
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
        let cur = self.current_priority.load(Ordering::SeqCst);
        self.rq.current_priority().max(Priority::from_raw(cur))
    }

    pub fn current_load(&self) -> u64 {
        self.rq.current_load()
            + if self.is_idle.load(Ordering::SeqCst) {
                0
            } else {
                1
            }
    }

    /// Whether this cpu is currently running its idle thread. Plain atomic read, so it is callable
    /// from contexts that must not take a lock (the mutex wait loop's stall report).
    pub fn is_idle(&self) -> bool {
        self.is_idle.load(Ordering::SeqCst)
    }

    /// Lock-free run-queue emptiness, for the same reason as [`Processor::is_idle`].
    pub fn rq_is_empty(&self) -> bool {
        self.rq.is_empty()
    }

    pub fn enter_idle(&self) {
        self.is_idle.store(true, Ordering::SeqCst);
    }

    pub fn exit_idle(&self) {
        self.is_idle.store(false, Ordering::SeqCst);
    }

    pub fn set_rebalance(&self) {
        self.must_rebalance.store(true, Ordering::SeqCst);
    }

    pub fn reset_rebalance(&self) {
        self.must_rebalance.store(false, Ordering::SeqCst);
    }

    pub fn must_rebalance(&self) -> bool {
        self.must_rebalance.load(Ordering::SeqCst)
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
        let mut tasks = self.ipi_tasks.lock();
        for task in tasks.drain(..) {
            (task.func)();
            task.outstanding.fetch_sub(1, Ordering::Release);
        }
    }

    pub fn push_exited(&self, th: ThreadRef) {
        self.exited.lock().push(th);
    }

    pub fn cleanup_exited(&self) {
        let item = self.exited.lock().pop();
        if let Some(item) = item {
            let _ = unsafe {
                Box::<ThreadRef, _>::from_raw(*item.self_reference.get().as_ref().unwrap())
            };
        }
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

pub const KERNEL_STACK_SIZE: usize = 512 * 1024 * 8; // 4M

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
