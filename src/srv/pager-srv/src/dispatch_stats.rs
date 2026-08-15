//! Latency of the hops between the kernel queueing a request and a worker starting on it.
//!
//! The kernel's own profile reports one segment for everything past its submit -- queue transit,
//! this dispatcher, the worker, the store, and the completion trip back -- so a request that sat in
//! the queue and a request that sat on the disk are indistinguishable from it. These split the
//! front of that segment into the two hops the pager controls:
//!
//! - **transit**: the kernel's submit stamp to the moment `kq_handler_main` dequeues it. Nobody has
//!   ever measured this; it is the one hop with no thread of its own, so it is also the one that a
//!   dequeue thread stuck somewhere else shows up in.
//! - **pickup**: dispatch to the worker taking the item off its channel. Already stamped by
//!   [`WorkItem`](crate::threads::WorkItem) and thrown at `trace!` per item, which is unreadable at
//!   the rates `pagepar` produces.
//!
//! `dispatch` and `inline` explain a bad transit rather than measuring a hop: the dequeue thread
//! handles non-fence evicts inline and batches up to 8, so an item can be sitting *in* the batch
//! that dequeued it while an evict ahead of it does synchronous work.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Upper bounds in microseconds; the last bucket is everything above.
const BOUNDS_US: [u64; 4] = [10, 100, 1_000, 10_000];
const NR_BUCKETS: usize = BOUNDS_US.len() + 1;

struct Hist {
    count: AtomicU64,
    sum_ns: AtomicU64,
    max_ns: AtomicU64,
    bucket: [AtomicU64; NR_BUCKETS],
}

impl Hist {
    const fn new() -> Self {
        Self {
            count: AtomicU64::new(0),
            sum_ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            bucket: [const { AtomicU64::new(0) }; NR_BUCKETS],
        }
    }

    fn record(&self, ns: u64) -> u64 {
        self.sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
        let idx = BOUNDS_US
            .iter()
            .position(|b| ns / 1000 <= *b)
            .unwrap_or(BOUNDS_US.len());
        self.bucket[idx].fetch_add(1, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn line(&self, name: &str) -> String {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return format!("{} none", name);
        }
        format!(
            "{} n={} mean={}us max={}us [<=10us {} <=100us {} <=1ms {} <=10ms {} >10ms {}]",
            name,
            count,
            self.sum_ns.load(Ordering::Relaxed) / count / 1000,
            self.max_ns.load(Ordering::Relaxed) / 1000,
            self.bucket[0].load(Ordering::Relaxed),
            self.bucket[1].load(Ordering::Relaxed),
            self.bucket[2].load(Ordering::Relaxed),
            self.bucket[3].load(Ordering::Relaxed),
            self.bucket[4].load(Ordering::Relaxed),
        )
    }
}

pub struct DispatchStats {
    transit: Hist,
    dispatch: Hist,
    /// Of `dispatch`, the part spent waiting for earlier items of the same batch to be dispatched.
    /// Split out because `dispatch` is measured from an item's *dequeue* stamp, so in a batch a
    /// later item inherits whatever its predecessors cost -- which reads as that item being slow
    /// to place when it never was.
    batchwait: Hist,
    probe: Hist,
    send: Hist,
    /// Own placement time for the fence-evict path, which bypasses `probe`/`send`: it picks a lane
    /// by hash and, since another lane would reorder the stream, blocks on that one rather than
    /// spilling.
    ordered: Hist,
    pickup_fast: Hist,
    pickup_bulk: Hist,
    inline: Hist,
    /// `transit`, restricted to `ObjectInfoReq`. The kernel blocks a whole map syscall on one of
    /// these (`lookup_object_and_wait`), and the aggregate `transit` cannot say whether an info
    /// request waits like the page-data traffic around it or is picked up promptly.
    info_transit: Hist,
    /// Dispatch to the lane worker starting on the info request, and then that worker to the
    /// detached task actually running. Both are thread hand-offs rather than work, and on a
    /// uniprocessor guest a hand-off is a wake plus a wait for a CPU -- which is the shape the
    /// kernel's 740 us blocked against 35 us of task points at.
    info_pickup: Hist,
    info_spawn: Hist,
    /// The info task itself, from the worker starting it to the completion going back, and the two
    /// things it does: the store's `len()` probe, and paging in the meta page. The second is a
    /// disk read for a stored object -- it is the reason an info request is not the cheap length
    /// probe `is_fast` still assumes it is.
    info_task: Hist,
    info_len: Hist,
    info_meta: Hist,
    /// Requests that arrived without a submit stamp. Should be zero; a nonzero value means transit
    /// is being reported off a subset, which is worth knowing before believing it.
    unstamped: AtomicU64,
    batches: AtomicU64,
    batch_items: AtomicU64,
    batch_max: AtomicU64,
    report_due: AtomicBool,
}

pub static DISPATCH_STATS: DispatchStats = DispatchStats {
    transit: Hist::new(),
    dispatch: Hist::new(),
    batchwait: Hist::new(),
    probe: Hist::new(),
    send: Hist::new(),
    ordered: Hist::new(),
    pickup_fast: Hist::new(),
    pickup_bulk: Hist::new(),
    inline: Hist::new(),
    info_transit: Hist::new(),
    info_pickup: Hist::new(),
    info_spawn: Hist::new(),
    info_task: Hist::new(),
    info_len: Hist::new(),
    info_meta: Hist::new(),
    unstamped: AtomicU64::new(0),
    batches: AtomicU64::new(0),
    batch_items: AtomicU64::new(0),
    batch_max: AtomicU64::new(0),
    report_due: AtomicBool::new(false),
};

impl DispatchStats {
    /// Monotonic nanoseconds, on the same clock the kernel stamps requests with.
    pub fn now_ns() -> u64 {
        twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_nanos() as u64
    }

    /// A request was dequeued from the kernel queue at `now`, having been submitted at `submit`.
    ///
    /// Marks a report due on a power of two but does not emit it: this is called per item inside
    /// the dequeue loop, within the window `dispatched` measures, and a console write here is
    /// charged to the dispatch it precedes. [`Self::report_if_due`] does the writing, after the
    /// batch.
    pub fn transit(&self, submit: Option<u64>, now: u64) {
        let Some(submit) = submit else {
            self.unstamped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        // Saturating rather than asserting monotonic: the two readings come from different CPUs,
        // and a few nanoseconds of TSC skew must not turn into a garbage bucket.
        let n = self.transit.record(now.saturating_sub(submit));
        if n.is_power_of_two() {
            self.report_due.store(true, Ordering::Relaxed);
        }
    }

    /// Transit for an `ObjectInfoReq`, recorded alongside the aggregate rather than instead of it.
    ///
    /// Silent about reports: the aggregate `transit` call next to this one already decides when a
    /// report is due, and marking it here as well would only change *which* item triggers it.
    pub fn info_transit(&self, submit: Option<u64>, now: u64) {
        if let Some(submit) = submit {
            self.info_transit.record(now.saturating_sub(submit));
        }
    }

    /// One whole info task, from the worker starting it to the completion being handed back.
    pub fn info_task(&self, ns: u64) {
        self.info_task.record(ns);
    }

    /// Dispatch to the lane worker picking the info request up.
    pub fn info_pickup(&self, ns: u64) {
        self.info_pickup.record(ns);
    }

    /// The lane worker reaching the spawn to the detached task starting.
    pub fn info_spawn(&self, ns: u64) {
        self.info_spawn.record(ns);
    }

    /// The two things a lookup does: the store's length probe, and the meta-page fetch. `meta` is
    /// `None` for an external object, whose meta page the kernel synthesizes from the length --
    /// which is the difference between an info request that touches the disk and one that does
    /// not, and the reason these are counted apart rather than summed.
    pub fn info_lookup(&self, len: u64, meta: Option<u64>) {
        self.info_len.record(len);
        if let Some(meta) = meta {
            self.info_meta.record(meta);
        }
    }

    /// Time this item spent waiting for earlier items of its batch to be dispatched.
    pub fn batchwait(&self, ns: u64) {
        self.batchwait.record(ns);
    }

    /// Placement time for a fence evict, which never spills to another lane.
    pub fn ordered(&self, ns: u64) {
        self.ordered.record(ns);
    }

    pub fn report_if_due(&self) {
        if self.report_due.swap(false, Ordering::Relaxed) {
            self.report();
        }
    }

    /// One batch of `items` requests drained from the kernel queue.
    pub fn batch(&self, items: usize) {
        self.batches.fetch_add(1, Ordering::Relaxed);
        self.batch_items.fetch_add(items as u64, Ordering::Relaxed);
        self.batch_max.fetch_max(items as u64, Ordering::Relaxed);
    }

    /// Time from the batch being dequeued to this item reaching a lane.
    pub fn dispatched(&self, ns: u64) {
        self.dispatch.record(ns);
    }

    /// The two halves of `dispatch`, so a slow one says *which*: `probe` is the `is_fast` lane
    /// decision, whose store probe is documented as a cache read the dequeue thread may make
    /// safely, and `send` is placing the item on a lane's channel, which blocks when every lane is
    /// full.
    pub fn probe(&self, ns: u64) {
        self.probe.record(ns);
    }

    pub fn send(&self, ns: u64) {
        self.send.record(ns);
    }

    /// Time a worker's item spent between dispatch and the worker starting on it.
    pub fn pickup(&self, fast: bool, ns: u64) {
        if fast {
            self.pickup_fast.record(ns);
        } else {
            self.pickup_bulk.record(ns);
        }
    }

    /// Time the dequeue thread spent handling a non-fence evict inline, with the rest of its batch
    /// waiting behind it.
    pub fn inline(&self, ns: u64) {
        self.inline.record(ns);
    }

    pub fn report(&self) {
        let batches = self.batches.load(Ordering::Relaxed).max(1);
        tracing::info!(
            "DISPATCH: {}; {}; {}; {}; {}; {}; {}; {}; {}; batches={} items/batch={} max={} \
             unstamped={}",
            self.transit.line("transit"),
            self.dispatch.line("dispatch"),
            self.batchwait.line("batchwait"),
            self.probe.line("probe"),
            self.send.line("send"),
            self.ordered.line("ordered"),
            self.pickup_fast.line("pickup-fast"),
            self.pickup_bulk.line("pickup-bulk"),
            self.inline.line("inline-evict"),
            batches,
            self.batch_items.load(Ordering::Relaxed) / batches,
            self.batch_max.load(Ordering::Relaxed),
            self.unstamped.load(Ordering::Relaxed),
        );
        tracing::info!(
            "DISPATCH-INFO: {}; {}; {}; {}; {}; {}",
            self.info_transit.line("transit"),
            self.info_pickup.line("pickup"),
            self.info_spawn.line("spawn"),
            self.info_task.line("task"),
            self.info_len.line("store-len"),
            self.info_meta.line("meta-page-in"),
        );
    }
}
