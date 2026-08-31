use std::{
    cell::Cell,
    ops::Range,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Condvar, Mutex, OnceLock,
    },
    thread::{available_parallelism, JoinHandle},
    time::{Duration, Instant},
};

use object_store::ProbeMiss;
use twizzler_abi::{
    object::ObjID,
    pager::{
        CompletionToKernel, KernelCommand, ObjectEvictFlags, ObjectRange, PagerFlags,
        RequestFromKernel,
    },
    syscall::{sys_thread_set_priority, PriorityClass, ThreadPriority},
};
use twizzler_queue::{QueueError, ReceiveFlags, SubmissionFlags};

use crate::{
    dispatch_stats::{DispatchStats, DISPATCH_STATS},
    nvme::controller::MAX_DATA_QUEUES,
    request_handle::handle_kernel_request,
    watchdog, PAGER_CTX,
};

/// Worker threads per core. Measured: at 1x the blocking NVMe leaf loses to the async one by 2.1x,
/// and at 2x it wins back the intra-worker overlap it gave up (pagerperf.md 1).
const WORKER_SCALE: usize = 2;

/// Scheduler priority for the pager's service threads.
///
/// The pager is on the critical path of every fault in the system, so a thread woken by a
/// completion and then queued behind ordinary userspace work adds its own scheduling latency to
/// that fault. These put pager threads ahead of ordinary threads (which run at
/// [`ThreadPriority::USER`]), and demand work ahead of bulk work -- the same ordering the fast/bulk
/// lane split expresses in the dispatcher, now expressed in the scheduler too.
///
/// Deliberately still `PriorityClass::User` rather than realtime. A pager thread is *not* more
/// important than the thread whose fault it is servicing -- the right answer is to inherit that
/// thread's priority (`pagerplan.md` stage 4), and a blanket realtime class would let a background
/// task's fault preempt a realtime thread, which is exactly what inheritance is supposed to
/// prevent. It also keeps a pager thread that spins (`InflightRequest::spin`) from locking an
/// ordinary thread off a core entirely.
///
/// Both are expressed as offsets from the default so "above ordinary userspace" stays true if that
/// default moves, and they are two kernel timeshare buckets apart (`MAX_PRIORITY / NR_QUEUES` = 16
/// wide), so the ordering between the lanes is real placement rather than a tiebreak.
const FAST_LANE_PRIORITY: ThreadPriority =
    ThreadPriority::new(PriorityClass::User, ThreadPriority::USER.value + 48);
const BULK_LANE_PRIORITY: ThreadPriority =
    ThreadPriority::new(PriorityClass::User, ThreadPriority::USER.value + 32);

/// Raise the calling thread's priority. Non-fatal: an unboosted pager is slower, not broken.
pub fn boost_priority(pri: ThreadPriority) {
    if let Err(e) = sys_thread_set_priority(ObjID::new(0), pri) {
        tracing::warn!("failed to set pager thread priority to {:?}: {}", pri, e);
    }
}

/// Priority for pager threads that are not lanes but gate them: the kernel-queue dequeue thread
/// (nothing is dispatched while it is off-cpu, and it handles non-fence evicts inline) and the
/// physical-read/write requester (single-outstanding, so every page-in's data movement waits on
/// it). Both are cheap and always on the demand path, so they run with the fast lanes.
pub const SERVICE_PRIORITY: ThreadPriority = FAST_LANE_PRIORITY;

/// Fast-lane depth at which a fast request starts eyeing an idle bulk lane.
///
/// One outstanding item is the lane doing its job -- borrowing there drains the reservation into
/// bulk on essentially every request, since `DepthGuard` keeps the count raised for the whole life
/// of a detached page-in. At two, something is genuinely queued behind the in-flight item, which is
/// the queueing the reservation exists to prevent.
const FAST_BORROW_DEPTH: usize = 2;

/// Most fast lanes to reserve, however large the pool gets. The reservation exists to keep small
/// demand faults off bulk transfers, which one or two lanes achieve; past that it is just bulk
/// capacity taken away.
const MAX_FAST_LANES: usize = 2;

/// Watchdog owner name for a lane, so a dump says which kind of worker wedged.
///
/// Leaked, once per worker at pool construction: the watchdog wants `'static` and the pool is built
/// exactly once, so this is a bounded handful of allocations rather than a growing leak. It used to
/// be a pair of fixed arrays, whose *lengths* silently became the cap on pool size.
fn lane_name(fast: bool, index: usize) -> &'static str {
    let name = if fast {
        format!("fast{}", index)
    } else {
        format!("worker{}", index)
    };
    Box::leak(name.into_boxed_str())
}

/// Largest *urgent segment* a fast lane will accept -- the pages a thread is blocked on, not the
/// whole range the kernel widened around them.
///
/// Measured against the widened range instead, this admitted nothing at all: `LANESTATS` reported
/// 0 of 32 page-data requests fast, 31 rejected on size, and 0 admitted even at a limit of 512
/// (`pagerplan.md` stage 4). That is because `ensure_in_core_pager` widens a one-page touch to a
/// 64-page run or a whole 512-page region, so the size test was reading the read-ahead and
/// rejecting the fault attached to it -- and a lane that only ever runs `ObjectInfoReq` and
/// `DramPages` is not a page-fault reservation at all.
///
/// What makes the urgent segment the right metric is that the pager transfers it first and
/// completes it separately, so it is the work that stands between the faulting thread and its
/// wakeup ([`crate::request_handle::urgent_segment`]). The read-ahead behind it is not on that
/// path. 16 pages (64 KiB) matches `REQUIRED_SEGMENT_LIMIT`, so what the cut actually excludes is
/// the case where the two differ: a required range served as a whole 2 MiB region for the sake of
/// the large-page merge, which buys no early wake and belongs on a bulk lane.
const FAST_PAGE_LIMIT: usize = 16;

/// Whether a request belongs on a reserved fast lane. `DramPages` is pure bookkeeping and
/// `ObjectInfoReq` is a `len()` probe the length cache usually answers without touching the disk
/// (see pagerperf.md 12). Page data qualifies only when the segment someone is *blocked on* is
/// small ([`FAST_PAGE_LIMIT`]), it is not a prefetch, it was not raised for a background thread,
/// and it is answerable without the fs lock. Prefetch ranges run to the whole
/// object, which is exactly the traffic the reservation exists to dodge; create/delete/evict all do
/// synchronous filesystem work under the global `fs` lock, so they are bulk by definition.
fn is_fast(req: &RequestFromKernel) -> bool {
    match req.cmd() {
        KernelCommand::ObjectInfoReq(_) | KernelCommand::DramPages(_) => true,
        KernelCommand::PageDataReq(id, range, flags, required) => {
            let flags_ok = !flags.intersects(PagerFlags::PREFETCH | PagerFlags::BACKGROUND);
            let urgent_range = crate::request_handle::urgent_segment(range, required);
            let urgent = urgent_range.page_count();
            let pages = range.page_count();
            // Probed on the whole range, not the urgent segment: the lane serves the read-ahead
            // inline after the urgent pages, so it parks on the fs lock if any part of the request
            // needs it.
            //
            // Deliberately not short-circuited: the question `FAST_PAGE_LIMIT` has never been able
            // to answer is whether raising it would *let anything through*, and that needs the
            // probe evaluated on the requests the size test currently rejects. Two cache reads on
            // the dequeue thread, which is the same cost the accepted path already pays.
            let miss = probe_store(id, range);
            let probe_ok = !miss.would_block();
            // The counterfactual the probe cannot otherwise answer: if the lane served only the
            // urgent segment and handed the read-ahead tail back to a bulk lane, would the fs lock
            // still be in the way? Only asked when the widened range was refused, so the common
            // path pays nothing extra.
            let urgent_probe_ok = probe_ok || !probe_store(id, urgent_range).would_block();
            LANE_STATS.record(flags_ok, urgent, pages, probe_ok, miss, urgent_probe_ok);
            flags_ok && urgent <= FAST_PAGE_LIMIT && probe_ok
        }
        KernelCommand::ObjectCreate(..) | KernelCommand::ObjectDel(_) => false,
        KernelCommand::ObjectEvict(_) => false,
    }
}

/// What the fast-lane reservation actually admits, and what a bigger [FAST_PAGE_LIMIT] would.
///
/// `pagerperf.md` 11 set the threshold against the request shapes `ensure_in_core_pager` is
/// *written* to emit and called it "a guess pending measurement"; this is that measurement. The
/// `would_be_fast_at_*` counters are the decisive ones: they hold the flags and probe tests fixed
/// and vary only the size limit, so the difference between them is exactly what raising it buys.
struct LaneStats {
    page_data: AtomicU64,
    fast: AtomicU64,
    rejected_flags: AtomicU64,
    rejected_size: AtomicU64,
    rejected_probe: AtomicU64,
    would_be_fast_at_64: AtomicU64,
    would_be_fast_at_512: AtomicU64,
    would_be_fast_unlimited: AtomicU64,
    /// [`Self::rejected_probe`] split by which cache came up short. `len` and `no_extents` mean
    /// the store must go to disk for this object; `partial` means it has the extents cached but
    /// was asked about a range they do not span.
    probe_len: AtomicU64,
    probe_no_extents: AtomicU64,
    probe_partial: AtomicU64,
    /// The decisive counterfactual: requests that a fast lane could take **today** if it served
    /// only the urgent segment and handed the read-ahead tail to a bulk lane. Flags and size held
    /// fixed, so this is exactly what tail re-dispatch would buy -- and if it is ~0, nothing short
    /// of the fs lock itself will open the fast lanes (`pagerplan.md` stages 4 and 6).
    would_be_fast_urgent_only: AtomicU64,
    pages_fast: AtomicU64,
    /// Of [`Self::pages_fast`], the pages nobody was blocked on -- the read-ahead a fast lane
    /// carries inline after the urgent segment it was admitted for. This is the cost of sizing
    /// admission on the urgent segment, and the number that decides whether the tail needs
    /// handing back to a bulk lane (`pagerplan.md` stage 4). Zero means the widened range and the
    /// urgent segment always coincided and the re-cut changed nothing.
    pages_fast_tail: AtomicU64,
    pages_total: AtomicU64,
    report_due: std::sync::atomic::AtomicBool,
}

static LANE_STATS: LaneStats = LaneStats {
    page_data: AtomicU64::new(0),
    fast: AtomicU64::new(0),
    rejected_flags: AtomicU64::new(0),
    rejected_size: AtomicU64::new(0),
    rejected_probe: AtomicU64::new(0),
    would_be_fast_at_64: AtomicU64::new(0),
    would_be_fast_at_512: AtomicU64::new(0),
    would_be_fast_unlimited: AtomicU64::new(0),
    probe_len: AtomicU64::new(0),
    probe_no_extents: AtomicU64::new(0),
    probe_partial: AtomicU64::new(0),
    would_be_fast_urgent_only: AtomicU64::new(0),
    pages_fast: AtomicU64::new(0),
    pages_fast_tail: AtomicU64::new(0),
    pages_total: AtomicU64::new(0),
    report_due: std::sync::atomic::AtomicBool::new(false),
};

impl LaneStats {
    /// `urgent` is what the size test judges (the pages a waiter is blocked on); `total` is what
    /// the lane ends up transferring for the request either way.
    fn record(
        &self,
        flags_ok: bool,
        urgent: usize,
        total: usize,
        probe_ok: bool,
        miss: ProbeMiss,
        urgent_probe_ok: bool,
    ) {
        let n = self.page_data.fetch_add(1, Ordering::Relaxed) + 1;
        self.pages_total.fetch_add(total as u64, Ordering::Relaxed);
        if !flags_ok {
            self.rejected_flags.fetch_add(1, Ordering::Relaxed);
        }
        if urgent > FAST_PAGE_LIMIT {
            self.rejected_size.fetch_add(1, Ordering::Relaxed);
        }
        if !probe_ok {
            self.rejected_probe.fetch_add(1, Ordering::Relaxed);
            match miss {
                ProbeMiss::Len => &self.probe_len,
                ProbeMiss::NoExtents => &self.probe_no_extents,
                ProbeMiss::Partial => &self.probe_partial,
                ProbeMiss::Cached => unreachable!("probe_ok is this same predicate"),
            }
            .fetch_add(1, Ordering::Relaxed);
        }
        if flags_ok && urgent_probe_ok && urgent <= FAST_PAGE_LIMIT {
            self.would_be_fast_urgent_only
                .fetch_add(1, Ordering::Relaxed);
        }
        if flags_ok && probe_ok {
            if urgent <= FAST_PAGE_LIMIT {
                self.fast.fetch_add(1, Ordering::Relaxed);
                self.pages_fast.fetch_add(total as u64, Ordering::Relaxed);
                self.pages_fast_tail
                    .fetch_add(total.saturating_sub(urgent) as u64, Ordering::Relaxed);
            }
            if urgent <= 64 {
                self.would_be_fast_at_64.fetch_add(1, Ordering::Relaxed);
            }
            if urgent <= 512 {
                self.would_be_fast_at_512.fetch_add(1, Ordering::Relaxed);
            }
            self.would_be_fast_unlimited.fetch_add(1, Ordering::Relaxed);
        }
        // Deferred rather than emitted here. This runs inside `is_fast`, which the dequeue thread
        // calls to classify a request -- and that call is a timed span (`DISPATCH: probe`). A
        // console write inside it is charged to the thing being measured, and on the first pass it
        // produced exactly as many millisecond `probe` outliers as there were reports, which is
        // indistinguishable from the store probe blocking. Nothing may log inside a timed span.
        if n.is_power_of_two() {
            self.report_due.store(true, Ordering::Relaxed);
        }
    }

    /// Emit a pending report. Called from the dequeue loop once a batch is fully dispatched, so the
    /// write lands outside every span.
    fn report_if_due(&self) {
        if !self.report_due.swap(false, Ordering::Relaxed) {
            return;
        }
        if !crate::watchdog::diag_enabled() {
            return;
        }
        tracing::info!(
            "LANESTATS: {} page-data ({} pages); fast {} ({} pages, {} of them read-ahead tail); rejected {} flags / {} size / {} probe ({} len, {} no-extents, {} partial); would be fast at urgent limit 64: {}, 512: {}, unlimited: {}; on urgent segment alone: {}",
            self.page_data.load(Ordering::Relaxed),
            self.pages_total.load(Ordering::Relaxed),
            self.fast.load(Ordering::Relaxed),
            self.pages_fast.load(Ordering::Relaxed),
            self.pages_fast_tail.load(Ordering::Relaxed),
            self.rejected_flags.load(Ordering::Relaxed),
            self.rejected_size.load(Ordering::Relaxed),
            self.rejected_probe.load(Ordering::Relaxed),
            self.probe_len.load(Ordering::Relaxed),
            self.probe_no_extents.load(Ordering::Relaxed),
            self.probe_partial.load(Ordering::Relaxed),
            self.would_be_fast_at_64.load(Ordering::Relaxed),
            self.would_be_fast_at_512.load(Ordering::Relaxed),
            self.would_be_fast_unlimited.load(Ordering::Relaxed),
            self.would_be_fast_urgent_only.load(Ordering::Relaxed),
        );
    }
}

/// Why serving this range would park the lane on the object store's fs lock, if it would.
///
/// That lock is global and held across NVMe round trips (pagerperf.md 2), so a fast lane that takes
/// it can sit behind a bulk transfer for a whole disk round trip -- the queueing the reservation
/// exists to prevent, and the one thing that makes running these lanes above ordinary userspace
/// unsafe: the holder can be of any priority class, and a userspace mutex donates nothing. Sending
/// such work to a bulk lane does not make *it* faster -- it waits for the same lock either way --
/// it keeps the fast lane available for the requests that need nothing but caches.
///
/// Asked on the dequeue thread, which nothing may stall, so it must stay a cache read: it takes
/// only the length and extent caches' own short locks and never touches the disk.
fn probe_store(id: ObjID, range: ObjectRange) -> ProbeMiss {
    PAGER_CTX.get().map_or(ProbeMiss::Cached, |ctx| {
        crate::helpers::page_in_would_block(ctx, id, range)
    })
}

pub struct WorkItem {
    start: Instant,
    qid: u32,
    req: RequestFromKernel,
}

impl WorkItem {
    fn new(qid: u32, req: RequestFromKernel) -> Self {
        Self {
            start: Instant::now(),
            qid,
            req,
        }
    }
}

pub struct WorkerThread {
    _handle: JoinHandle<()>,
    pending: async_channel::Sender<WorkItem>,
    /// Work owed by this lane: queued items plus items being handled plus any task they detached.
    /// Read by the dequeue thread to pick a lane, so it is deliberately not the channel length --
    /// see [`DepthGuard`].
    depth: Arc<AtomicUsize>,
}

/// This worker's nvme queue pair, so workers never contend on one submission queue.
///
/// Non-worker threads report 0 and share worker 0's queue. That is deliberate rather than a
/// fallback: the dequeue thread only handles non-fence evicts inline, which just record a range and
/// never touch the disk, and init is single-threaded, so neither issues enough traffic to earn a
/// queue.
#[thread_local]
static QUEUE_INDEX: Cell<usize> = Cell::new(0);

pub fn current_queue_index() -> usize {
    QUEUE_INDEX.get()
}

/// Number of worker threads, fixed on first call.
///
/// Memoized because the nvme controller sizes its queue pairs from this and is initialized before
/// the pool exists -- the two have to agree, and `available_parallelism` is not contractually
/// stable across calls.
/// How many nvme queue pairs to ask the device for.
///
/// `WORKER_SCALE` per core, capped by `MAX_DATA_QUEUES`. Memoized because the controller is
/// initialized before the pool exists and the two have to agree, and because
/// `available_parallelism` is not contractually stable across calls.
///
/// A blocking worker is parked for the whole transfer, so the pool -- not the executor -- is what
/// keeps commands outstanding; 1x cores measured 2.1x worse than 2x (pagerperf.md 1). Floor of 2 so
/// the fast lane always has somewhere to live.
pub fn desired_queues() -> usize {
    static NR: OnceLock<usize> = OnceLock::new();
    *NR.get_or_init(|| {
        (available_parallelism().unwrap().get() * WORKER_SCALE).clamp(2, MAX_DATA_QUEUES)
    })
}

/// Queue pairs the device actually granted, recorded by the controller during init.
static GRANTED_QUEUES: OnceLock<usize> = OnceLock::new();

/// Record the negotiated queue count. Called once from `nvme::controller`, before the pool is
/// built.
pub fn set_granted_queues(n: usize) {
    let _ = GRANTED_QUEUES.set(n);
}

/// Number of worker threads: **one per nvme queue pair**.
///
/// A waiting thread reaps the queue it submitted to, so 1-1 gives each worker an uncontended
/// requester lock and no spurious wakeups -- with several workers per queue, one queue's interrupt
/// wakes them all and most find the completion belongs to someone else. Sharing is *correct*
/// (`pagerplan.md`, "waiters reap"), just noisier, which is why this follows the device down rather
/// than leaving the pool at what we asked for.
///
/// `desired_queues()` is the fallback for configurations with no nvme controller at all (the
/// virtio-mem store), where nothing ever records a grant.
pub fn nr_workers() -> usize {
    GRANTED_QUEUES
        .get()
        .copied()
        .unwrap_or_else(desired_queues)
        .max(2)
}

/// Holds a lane's depth raised for the lifetime of one unit of work.
///
/// The reason this exists rather than a bare fetch_add/fetch_sub pair: page-data and fence-sync
/// requests used to detach a task and return, so the work item finished while the paging it asked
/// for was still running. With blocking workers the item and the work are the same thing, so the
/// count is now just "queued plus in progress" -- `pagerplan.md` stage 3 collapses it further.
pub struct DepthGuard(Option<Arc<AtomicUsize>>);

impl DepthGuard {
    /// Take over a count already raised by the dispatcher when it queued the item.
    fn adopt(depth: Arc<AtomicUsize>) -> Self {
        Self(Some(depth))
    }
}

impl Drop for DepthGuard {
    fn drop(&mut self) {
        if let Some(depth) = self.0.take() {
            depth.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

impl WorkerThread {
    fn new(index: usize, name: &'static str, fast: bool) -> Self {
        let (send, recv) = async_channel::bounded::<WorkItem>(32);
        let depth = Arc::new(AtomicUsize::new(0));
        let thread_depth = depth.clone();
        Self {
            _handle: std::thread::spawn(move || {
                boost_priority(if fast {
                    FAST_LANE_PRIORITY
                } else {
                    BULK_LANE_PRIORITY
                });
                QUEUE_INDEX.set(index);
                loop {
                    let wi = recv.recv_blocking().unwrap();
                    let _depth = DepthGuard::adopt(thread_depth.clone());
                    // Labelled by the *lane's* class, not the request's: a fast request that
                    // borrowed an idle bulk lane waited in that lane's queue, which is what this
                    // measures.
                    DISPATCH_STATS.pickup(fast, wi.start.elapsed().as_nanos() as u64);
                    if matches!(wi.req.cmd(), KernelCommand::ObjectInfoReq(_)) {
                        DISPATCH_STATS.info_pickup(wi.start.elapsed().as_nanos() as u64);
                    }
                    tracing::trace!(
                        "{}: starting handling after {}us",
                        wi.qid,
                        wi.start.elapsed().as_micros()
                    );

                    let work = watchdog::begin(name, wi.qid, wi.req);
                    let resp =
                        handle_kernel_request(PAGER_CTX.get().unwrap(), wi.qid, wi.req, &work);
                    tracing::trace!(
                        "{}: done handling after {}us",
                        wi.qid,
                        wi.start.elapsed().as_micros()
                    );
                    work.phase("notify-kernel");
                    if let Some(resp) = resp {
                        PAGER_CTX
                            .get()
                            .unwrap()
                            .kernel_notify
                            .complete(wi.qid, resp, SubmissionFlags::empty())
                            .unwrap();
                    }
                }
            }),
            pending: send,
            depth,
        }
    }

    /// Charge the lane before queueing, so a dispatch decision can never race ahead of the item it
    /// just placed. Gives the item back if the lane is full.
    fn try_send(&self, wi: WorkItem) -> Result<(), WorkItem> {
        self.depth.fetch_add(1, Ordering::Relaxed);
        self.pending.try_send(wi).map_err(|e| {
            self.depth.fetch_sub(1, Ordering::Relaxed);
            e.into_inner()
        })
    }

    fn send_blocking(&self, wi: WorkItem) {
        self.depth.fetch_add(1, Ordering::Relaxed);
        self.pending.send_blocking(wi).expect("pager worker exited");
    }
}

pub struct Workers {
    threads: Vec<WorkerThread>,
    /// `threads[..nr_fast]` are reserved for requests [`is_fast`] accepts, so a one-page fault
    /// never queues behind a multi-megabyte prefetch. Fixed at startup, which is what lets the
    /// fence hash in `dispatch_ordered` stay stable for the life of the process.
    nr_fast: usize,
    /// Rotating start for the idle-bulk-lane search, so a burst of borrows spreads instead of
    /// stacking on the lowest-index lane. The depth charge lands after the decision is read, so a
    /// fixed start makes concurrent dispatches all pick the same "idle" lane.
    borrow_rotor: AtomicUsize,
}

impl Workers {
    fn new() -> Self {
        let nr_threads = nr_workers();
        // One reserved lane keeps cheap requests moving; a second only starts paying once there are
        // enough bulk lanes left that reserving it does not just move the queue. Never take the
        // last lane -- bulk work has to have somewhere to go.
        let nr_fast = (nr_threads / 4)
            .clamp(1, MAX_FAST_LANES)
            .min(nr_threads - 1);
        let threads = (0..nr_threads)
            .map(|index| {
                let fast = index < nr_fast;
                let name = if fast {
                    lane_name(true, index)
                } else {
                    lane_name(false, index - nr_fast)
                };
                WorkerThread::new(index, name, fast)
            })
            .collect();
        Self {
            threads,
            nr_fast,
            borrow_rotor: AtomicUsize::new(0),
        }
    }

    /// An idle bulk lane, starting the search at a rotating offset.
    fn idle_bulk_lane(&self) -> Option<usize> {
        let bulk = self.lane(false);
        let n = bulk.len();
        if n == 0 {
            return None;
        }
        let start = self.borrow_rotor.fetch_add(1, Ordering::Relaxed) % n;
        (0..n)
            .map(|k| bulk.start + (start + k) % n)
            .find(|i| self.threads[*i].depth.load(Ordering::Relaxed) == 0)
    }

    fn lane(&self, fast: bool) -> Range<usize> {
        if fast {
            0..self.nr_fast
        } else {
            self.nr_fast..self.threads.len()
        }
    }

    /// Queue an item on the least-loaded lane of its class. Depth counts detached tasks, so a
    /// worker that already returned from its work item but is still paging for it still reads as
    /// loaded; ties break to the lowest index, and the charge lands before the next decision reads
    /// it, so a run of identical requests still spreads.
    fn dispatch(&self, wi: WorkItem) {
        let probe_start = DispatchStats::now_ns();
        let fast = is_fast(&wi.req);
        let send_start = DispatchStats::now_ns();
        DISPATCH_STATS.probe(send_start - probe_start);
        self.place(wi, fast);
        DISPATCH_STATS.send(DispatchStats::now_ns() - send_start);
    }

    /// Place an already-classified item on a lane. Split out from [`Self::dispatch`] only so the
    /// lane decision and the placement can be timed apart -- the `is_fast` probe reads the object
    /// store's caches, and whether a slow dispatch is that probe or a full lane is the whole
    /// question.
    fn place(&self, wi: WorkItem, fast: bool) {
        let lane = self.lane(fast);
        let depth = |i: usize| self.threads[i].depth.load(Ordering::Relaxed);
        let mut best = lane
            .clone()
            .min_by_key(|i| depth(*i))
            .expect("every request class needs at least one lane");
        // A fast request waiting behind another fast request is the queueing the reservation was
        // meant to prevent, and a bulk lane at depth 0 is not the bulk transfer it exists to get
        // out from behind -- so borrow it. Only in this direction: bulk work never takes a fast
        // lane, or the reservation would stop meaning anything.
        if fast && depth(best) >= FAST_BORROW_DEPTH {
            if let Some(idle) = self.idle_bulk_lane() {
                best = idle;
            }
        }
        // Spill before blocking: an idle sibling beats stalling the dequeue thread, and stalling
        // beats the panic a failed `try_send` used to be. Fast work may fall back onto *idle* bulk
        // lanes too -- an empty one is not the bulk transfer the reservation exists to avoid -- but
        // never onto a busy one, which would be the queueing it exists to prevent.
        let mut wi = wi;
        let siblings = lane.filter(|i| *i != best);
        let idle_bulk = self
            .lane(false)
            .filter(|i| fast && *i != best && depth(*i) == 0);
        for idx in std::iter::once(best).chain(siblings).chain(idle_bulk) {
            match self.threads[idx].try_send(wi) {
                Ok(()) => return,
                Err(item) => wi = item,
            }
        }
        self.threads[best].send_blocking(wi);
    }

    /// Queue an item on a lane chosen by `hash` instead of by depth, for streams that have to stay
    /// ordered. Never spills, since another lane would reorder it -- it waits for its own.
    fn dispatch_ordered(&self, wi: WorkItem, hash: u64) {
        let lane = self.lane(false);
        let idx = lane.start + (hash as usize) % lane.len();
        if let Err(wi) = self.threads[idx].try_send(wi) {
            self.threads[idx].send_blocking(wi);
        }
    }
}

pub struct PagerThreadPool {
    _workers: Arc<Workers>,
    _kq_handler: JoinHandle<()>,
}

impl PagerThreadPool {
    pub fn new(
        queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
    ) -> Self {
        let pool = Arc::new(Workers::new());
        PagerThreadPool {
            _workers: pool.clone(),
            _kq_handler: std::thread::spawn(move || kq_handler_main(pool, queue)),
        }
    }
}

/// Nothing here parks on a future any more.
///
/// This is where the executor was: a thread-local `LocalExecutor`, a hand-written parker
/// (`park_poll`) driving it against this thread's nvme interrupt word, `spawn_async` to detach a
/// page-in so the worker could take the next item, and `run_isolated` to poll one future without
/// the executor -- that last one existing only to keep lwext4's callbacks from re-entering it and
/// deadlocking on `Ext4Store::fs` (pagerperf.md 2). A blocking worker does the work on the thread
/// that took the item, so the handoff, the parker and the whole deadlock class go with it
/// (`pagerplan.md` stage 2). What replaced the parker is `InflightRequest::wait_owned`, which
/// sleeps on the flags word and this thread's queue interrupt together and reaps for itself.

fn kq_handler_main(
    workers: Arc<Workers>,
    queue: &'static twizzler_queue::Queue<RequestFromKernel, CompletionToKernel>,
) {
    boost_priority(SERVICE_PRIORITY);
    loop {
        // Each entry carries the moment it came off the queue, which is both the end of its transit
        // and the start of its wait for a lane. Stamped per item rather than per batch because a
        // batch is drained non-blockingly after the first item wakes this thread, so the last of
        // eight has been in the queue measurably less long than the first.
        let mut tmp = heapless::Vec::<(u32, RequestFromKernel, u64), 8>::new();
        while !tmp.is_full() {
            let res = queue.receive(ReceiveFlags::NON_BLOCK);
            match res {
                Ok((id, req)) => unsafe { tmp.push_unchecked((id, req, DispatchStats::now_ns())) },
                Err(e) if e == QueueError::WouldBlock => {
                    if !tmp.is_empty() {
                        break;
                    }
                    if let Ok((id, req)) = queue.receive(ReceiveFlags::empty()) {
                        unsafe { tmp.push_unchecked((id, req, DispatchStats::now_ns())) };
                    }
                }
                Err(e) => {
                    tracing::error!("queue recieve error: {}", e);
                }
            }
        }

        DISPATCH_STATS.batch(tmp.len());
        for (id, req, dequeued) in tmp {
            DISPATCH_STATS.transit(req.submit_ns(), dequeued);
            if matches!(req.cmd(), KernelCommand::ObjectInfoReq(_)) {
                DISPATCH_STATS.info_transit(req.submit_ns(), dequeued);
            }
            // Everything before this point in the loop body is bookkeeping, but everything before
            // it in *earlier iterations* is real dispatch work this item has been waiting through.
            // `ready` separates the two, so a slow placement can be told from a slow batch-mate.
            let ready = DispatchStats::now_ns();
            DISPATCH_STATS.batchwait(ready - dequeued);
            // Evicts never go through `dispatch`: a non-fence evict only records the page in
            // `PerObjectInner::sync_map`, and the fence that follows it drains and writes out what
            // was recorded, so the record has to happen first. Handling non-fence evicts inline
            // here is what guarantees that -- they complete before anything later in this batch is
            // even queued.
            if let KernelCommand::ObjectEvict(evict) = req.cmd() {
                // Send all per-object evict streams to one thread so they preserve order.
                if evict.flags.contains(ObjectEvictFlags::FENCE) {
                    let hash = req.id().map_or(0, |x| x.parts()[0] ^ x.parts()[1]) + id as u64;
                    workers.dispatch_ordered(WorkItem::new(id, req), hash);
                    let now = DispatchStats::now_ns();
                    DISPATCH_STATS.ordered(now - ready);
                    DISPATCH_STATS.dispatched(now - dequeued);
                } else {
                    // Handled inline, so anything that blocks here stops the pager dequeuing from
                    // the kernel at all -- worth naming separately from a stuck worker.
                    let work = watchdog::begin("kq-handler-inline", id, req);
                    let resp = handle_kernel_request(PAGER_CTX.get().unwrap(), id, req, &work);
                    work.phase("notify-kernel");
                    if let Some(resp) = resp {
                        PAGER_CTX
                            .get()
                            .unwrap()
                            .kernel_notify
                            .complete(id, resp, SubmissionFlags::empty())
                            .unwrap();
                    }
                    DISPATCH_STATS.inline(DispatchStats::now_ns() - ready);
                }
            } else {
                workers.dispatch(WorkItem::new(id, req));
                // After the dispatch returns, so a lane full enough to make `dispatch` block shows
                // up here rather than silently in the next item's transit.
                DISPATCH_STATS.dispatched(DispatchStats::now_ns() - dequeued);
            }
        }

        // Both reports emit here, with the batch fully dispatched and no span open. Emitting them
        // where the counters are updated put a console write inside `probe` and inside the
        // `dispatched` window, which is what the first two rounds of these numbers were measuring.
        LANE_STATS.report_if_due();
        DISPATCH_STATS.report_if_due();
    }
}

/// How long any blocking waiter in the pager sleeps before rechecking its own predicate.
///
/// Not a timeout in the usual sense -- nothing below gives up when it fires, they all loop until
/// their condition holds. It bounds the damage a lost wake can do, and it is the only way to
/// *detect* one: a wait that ends on this fallback with its condition **already true** is a
/// notification that should have arrived and did not.
///
/// The concrete reason it exists is one bug's worth, not a general distrust of the runtime:
/// `PagerData::free_page` returned pages to the pool without signalling `mem_avail`, so a thread
/// parked for memory could not be woken by another thread freeing some. That is fixed at the
/// source, and this is the backstop for the next one of its kind -- the async version of these
/// waiters was covered incidentally by an executor polling other tasks on the same thread, and
/// nothing covers them now. **The expected count is zero**, and a run that reports zero is
/// evidence the fallback is inert rather than load-bearing.
///
/// 500 ms is a bound on latency in an already-degraded state (a pager thread that would otherwise
/// be parked forever), deliberately far above anything on a working path, so it cannot turn into
/// a poll loop if the count is ever nonzero for a benign reason.
///
/// **Known blind spot, and it inverts the reading above.** This detector is itself a timed wait.
/// Against a failure class that kills *timed* waits -- a compartment where `thread::sleep` and
/// poll timeouts stop returning while event-driven paths keep running, which pid 852030 measured
/// on this runtime the same night this was written -- it cannot fire, because it is waiting on the
/// same broken primitive. So a silent `WAKEWATCH` does **not** establish "no missed wakes"; it is
/// equally the signature of the detector being in the same coma as the thing it watches. It only
/// reports on wakes lost while timers still work. An instrument that can only ever confirm is the
/// shape to distrust; treat zero here as untested until something independent of timers agrees.
pub const WAKE_FALLBACK: Duration = Duration::from_millis(500);

/// Counts what [`WAKE_FALLBACK`] catches.
///
/// `missed` is the one that matters: a waiter whose condition was already satisfied when its
/// sleep timed out. It should be zero, and every increment is a wake that went missing between a
/// notifier and a waiter holding the same mutex.
pub struct WakeWatch {
    fallbacks: AtomicU64,
    missed: AtomicU64,
}

pub static WAKE_WATCH: WakeWatch = WakeWatch {
    fallbacks: AtomicU64::new(0),
    missed: AtomicU64::new(0),
};

impl WakeWatch {
    /// Record a sleep that ended on the fallback rather than on a notification. `satisfied` is
    /// whether the condition was true at that moment -- i.e. whether a wake was owed.
    pub fn fallback(&self, satisfied: bool, what: &str) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
        if satisfied {
            let n = self.missed.fetch_add(1, Ordering::Relaxed) + 1;
            // Powers of two: a systematically-dropped wake would otherwise flood the console with
            // the evidence and slow down the thing being diagnosed.
            if n.is_power_of_two() {
                tracing::warn!(
                    "WAKEWATCH: {} woke on the {}ms fallback with its condition already met ({} such, {} fallbacks total) -- a notify was owed and not delivered",
                    what,
                    WAKE_FALLBACK.as_millis(),
                    n,
                    self.fallbacks.load(Ordering::Relaxed),
                );
            }
        }
    }
}

/// A one-shot slot: one thread blocks on it, another fills it exactly once.
///
/// Was a `Waker` in an `Option` and a `Future` impl on `&Waiter<T>`. The waking side is unchanged
/// -- it takes the lock, stores the item, releases the waiter -- so this is the same handoff with
/// a condvar in place of the executor that used to notice it.
pub struct Waiter<T: Send> {
    data: Mutex<Option<T>>,
    ready: Condvar,
}

impl<T: Send> Default for Waiter<T> {
    fn default() -> Self {
        Self {
            data: Mutex::new(None),
            ready: Condvar::new(),
        }
    }
}

impl<T: Send> Waiter<T> {
    pub fn finish(&self, item: T) {
        let mut data = self.data.lock().unwrap();
        *data = Some(item);
        drop(data);
        self.ready.notify_all();
    }

    /// Block until [`Self::finish`] has run, and take what it left.
    pub fn wait(&self) -> T {
        let mut data = self.data.lock().unwrap();
        loop {
            if let Some(item) = data.take() {
                return item;
            }
            let (guard, timeout) = self.ready.wait_timeout(data, WAKE_FALLBACK).unwrap();
            data = guard;
            if timeout.timed_out() {
                WAKE_WATCH.fallback(data.is_some(), "physrw completion");
            }
        }
    }
}
