use alloc::collections::btree_map::BTreeMap;
use core::{
    cell::UnsafeCell,
    hint::unlikely,
    sync::atomic::{
        AtomicBool, AtomicU64, AtomicUsize,
        Ordering::{self, Relaxed, SeqCst},
    },
};

use twizzler_abi::{
    object::ObjID,
    syscall::TraceSpec,
    trace::{TraceEntryFlags, TraceEntryHead, TraceKind},
};
use twizzler_rt_abi::error::{ObjectError, TwzError};

use super::{buffered_trace_data::BufferedTraceData, sink::TraceSink};
use crate::{
    condvar::CondVar,
    mutex::Mutex,
    once::Once,
    spinlock::Spinlock,
    thread::{ThreadRef, current_thread_ref, entry::start_new_kernel, priority::Priority},
};

#[derive(Debug)]
pub struct TraceEvent<T: Copy + core::fmt::Debug = ()> {
    header: TraceEntryHead,
    data: Option<T>,
}

impl TraceEvent<()> {
    pub fn new(mut head: TraceEntryHead) -> Self {
        head.flags.remove(TraceEntryFlags::HAS_DATA);
        Self {
            header: head,
            data: None,
        }
    }
}

impl<T: Copy + core::fmt::Debug> TraceEvent<T> {
    fn split(&self) -> (TraceEntryHead, BufferedTraceData) {
        (
            self.header,
            self.data
                .map(|data| BufferedTraceData::new(data))
                .unwrap_or_default(),
        )
    }

    fn split_async(self) -> (TraceEntryHead, BufferedTraceData) {
        (
            self.header,
            self.data
                .map(|data| BufferedTraceData::new_inline(data))
                .flatten()
                .unwrap_or_default(),
        )
    }

    pub fn new_with_data(mut head: TraceEntryHead, data: T) -> Self {
        head.flags.insert(TraceEntryFlags::HAS_DATA);
        Self {
            header: head,
            data: Some(data),
        }
    }
}

/// Writer wakes asked for, against wakes actually performed. The gap is what
/// [`TraceMgr::signal_work`]'s coalescing removes.
pub mod signalstats {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static ASKED: AtomicU64 = AtomicU64::new(0);
    static SENT: AtomicU64 = AtomicU64::new(0);

    static SINK_SIGNALS: AtomicU64 = AtomicU64::new(0);

    /// Announcements actually made to the consumer, against the events that produced them.
    pub fn sink_signal() {
        SINK_SIGNALS.fetch_add(1, Relaxed);
    }

    pub fn coalesced() {
        ASKED.fetch_add(1, Relaxed);
    }

    pub fn delivered() {
        ASKED.fetch_add(1, Relaxed);
        SENT.fetch_add(1, Relaxed);
    }

    pub fn print() {
        let asked = ASKED.load(Relaxed);
        if asked == 0 {
            return;
        }
        let sent = SENT.load(Relaxed);
        logln!(
            "== trace writer wakes: {} events signalled, {} wakes sent ({}% coalesced) ==",
            asked,
            sent,
            100 - (sent * 100 / asked)
        );
        logln!(
            "== trace sink announcements to the consumer: {} ==",
            SINK_SIGNALS.load(Relaxed)
        );
    }
}

/// The kernel-side cost budget of tracing, in counts rather than wall clock.
///
/// Wall clock cannot separate this: traced builds ran 29.20-35.25s against untraced 29.10-32.15s,
/// and `aspace switches` swings 1.2M-6.4M for identical code. Counts are exact, so the budget can
/// be computed instead of measured: events x per-event cost, with the per-event cost of the one
/// expensive call (`Instant::now`) measured once by a loop rather than per record.
pub mod enqueuestats {
    use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

    static EVENTS: AtomicU64 = AtomicU64::new(0);
    /// Spins because another cpu held the odd/even buffer lock -- real enqueue contention.
    static SPIN_BUSY: AtomicU64 = AtomicU64::new(0);
    /// Spins because the claiming compare-exchange lost -- also contention, counted apart so the
    /// two failure modes of the same single atomic can be told from each other.
    static SPIN_CAS: AtomicU64 = AtomicU64::new(0);
    static DROPS: AtomicU64 = AtomicU64::new(0);
    /// `new_trace_entry` calls, i.e. `Instant::now()` calls on the trace path.
    static ENTRIES: AtomicU64 = AtomicU64::new(0);
    /// Nanoseconds for one `Instant::now()`, measured once at shutdown.
    static NOW_NS_X1000: AtomicU64 = AtomicU64::new(0);

    pub fn event() {
        EVENTS.fetch_add(1, Relaxed);
    }

    pub fn entry() {
        ENTRIES.fetch_add(1, Relaxed);
    }

    pub fn spin_busy() {
        SPIN_BUSY.fetch_add(1, Relaxed);
    }

    pub fn spin_cas() {
        SPIN_CAS.fetch_add(1, Relaxed);
    }

    pub fn drop_event() {
        DROPS.fetch_add(1, Relaxed);
    }

    /// Time `Instant::now()` over a long loop, so the two bracketing reads are amortised away
    /// rather than being the thing measured.
    fn measure_now() -> u64 {
        const N: u64 = 100_000;
        let start = crate::instant::Instant::now();
        for _ in 0..N {
            core::hint::black_box(crate::instant::Instant::now());
        }
        let end = crate::instant::Instant::now();
        ((end - start).as_nanos() as u64 * 1000) / N
    }

    pub fn print() {
        let entries = ENTRIES.load(Relaxed);
        if entries == 0 {
            return;
        }
        let now_ns_x1000 = measure_now();
        NOW_NS_X1000.store(now_ns_x1000, Relaxed);
        let events = EVENTS.load(Relaxed);
        logln!(
            "== trace enqueue: {} entries built, {} enqueued, {} dropped; spins {} busy / {} cas ==",
            entries,
            events,
            DROPS.load(Relaxed),
            SPIN_BUSY.load(Relaxed),
            SPIN_CAS.load(Relaxed),
        );
        logln!(
            "  Instant::now() = {}.{:03} ns; {} calls on the trace path = {} us total",
            now_ns_x1000 / 1000,
            now_ns_x1000 % 1000,
            entries,
            (entries * now_ns_x1000) / 1_000_000,
        );
    }
}

/// Events allowed to accumulate in the async buffer before the writer is woken.
///
/// The writer was woken per enqueue, which is where its cost lives: measured at 13,449 wakes for
/// ~17,800 events -- roughly one per 1.3 events -- and ~12% of a build's on-cpu time, the largest
/// consumer after rustc itself. Each wake runs the whole loop: the sleeping `map` mutex,
/// `drain_async` under the odd/even spin lock, `write_all` per sink, the `has_work` lock and a
/// condvar wait. Collapsing the three object *writes* per record to one changed nothing (11.10%
/// -> 10.76%), which is what identified the per-wake path rather than the per-record path as the
/// cost.
///
/// Past the watermark every further event signals, and [`TraceMgr::signal_work`] coalesces those
/// into one condvar wake -- so the buffer cannot fill while the writer is already on its way.
/// Nothing strands: `add_sink` and `remove_sink` both drain, and removal now announces too.
///
/// The tradeoff is delivery latency for a *low-rate* trace: up to `WATERMARK - 1` events can sit
/// until the next one arrives. Sampling produces one event per tick per running thread, so under
/// a profile they flow continuously; a sparse event spec is the case to watch.
const ASYNC_SIGNAL_WATERMARK: usize = 64;

/// Coalesce writer wakes in [`TraceMgr::signal_work`]. A const so the arms differ by one line and
/// an A/B runs from one tree state.
const COALESCE_WAKES: bool = true;

const MAX_QUICK_ENABLED: usize = 10;
const MAX_PENDING_ASYNC: usize = 1024;
const MAX_SINK_PENDING: usize = 4096;

pub struct TraceMgr {
    map: Mutex<BTreeMap<ObjID, TraceSink>>,
    quick_enabled: [AtomicU64; MAX_QUICK_ENABLED],
    async_buffer: UnsafeCell<[Option<(TraceEntryHead, BufferedTraceData)>; MAX_PENDING_ASYNC]>,
    async_idx: AtomicUsize,
    async_overflow: AtomicBool,
    has_work: Spinlock<bool>,
    cv: CondVar,
}

unsafe impl Sync for TraceMgr {}
unsafe impl Send for TraceMgr {}

const _Z: AtomicU64 = AtomicU64::new(0);
const __Z: Option<(TraceEntryHead, BufferedTraceData)> = None;
pub static TRACE_MGR: TraceMgr = TraceMgr {
    map: Mutex::new(BTreeMap::new()),
    quick_enabled: [_Z; MAX_QUICK_ENABLED],
    async_buffer: UnsafeCell::new([__Z; MAX_PENDING_ASYNC]),
    async_idx: AtomicUsize::new(0),
    has_work: Spinlock::new(false),
    async_overflow: AtomicBool::new(false),
    cv: CondVar::new(),
};

static WRITE_THREAD: Once<ThreadRef> = Once::new();

impl TraceMgr {
    /// Tell the writer thread there is work, at most once per batch it has yet to consume.
    ///
    /// One wake covers any number of queued events, because [Self::drain_async] takes the whole
    /// async buffer in one pass -- so every signal after the first, until the writer clears the
    /// flag, wakes a thread that was already going to see this event. No wakeup is lost: the writer
    /// clears the flag under the same spinlock and re-drains before sleeping, so an enqueuer that
    /// observes `true` is guaranteed the writer has not yet decided to sleep, and one that observes
    /// `false` signals.
    ///
    /// **Measured, and it buys nothing.** The theory was that this is the dominant cost of tracing:
    /// the signal runs on the *enqueuing* thread and `CondVar::signal` is `enter_critical` + the
    /// condvar spinlock + `requeue_all()` + the critical-guard drop, so every record ran a
    /// scheduler requeue. Interleaved A/B from one tree state (`abtr`, round 1, identical source
    /// fingerprint):
    ///
    /// | arm | build | aspace switches | wakes sent |
    /// |---|---|---|---|
    /// | off | 31.95s | 4,113,769 | 21,556 (0% coalesced) |
    /// | on  | 31.95s | 4,149,798 | 17,759 (15% coalesced) |
    ///
    /// Identical wall clock and marginally *more* switches. The coalescing does what it says --
    /// 15% of wakes elided -- and the writer drains fast enough that the other 85% still find the
    /// flag clear, so there was never a large batch to collapse. Left on because it is strictly
    /// less work and provably correct, not because it was shown to help.
    ///
    /// It also does not explain the run where traced switches fell 6,380,681 -> 1,200,915; that was
    /// variance plus peers' changes landing between two builds. `aspace switches` swings 1.2M-6.4M
    /// for identical code, which makes it far too noisy to diagnose this on its own.
    fn signal_work(&self) {
        let mut sig = self.has_work.lock();
        if COALESCE_WAKES && *sig {
            signalstats::coalesced();
            return;
        }
        *sig = true;
        drop(sig);
        signalstats::delivered();
        self.cv.signal();
    }

    fn update_quick_enabled(&self, kind: TraceKind, events: u64) {
        let idx = kind as usize;
        if unlikely(idx >= MAX_QUICK_ENABLED) {
            return;
        }

        self.quick_enabled[idx].store(events, Relaxed);
    }

    #[inline]
    pub fn any_enabled(&self, kind: TraceKind, events: u64) -> bool {
        let idx = kind as usize;
        if unlikely(idx >= MAX_QUICK_ENABLED) {
            return true;
        }

        self.quick_enabled[idx].load(Relaxed) & events != 0
    }

    pub fn enqueue<T: Copy + core::fmt::Debug>(&self, event: TraceEvent<T>) {
        let mut map = self.map.lock();
        self.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.accepts(&event.header) {
                sink.enqueue(event.split());
            }
        }
        drop(map);
        self.signal_work();
    }

    pub fn process_async_and_maybe_flush(&self) {
        let mut map = self.map.lock();
        self.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.pending() >= MAX_SINK_PENDING {
                sink.write_all();
            }
        }
    }

    pub fn async_enqueue<T: Copy + core::fmt::Debug>(&self, event: TraceEvent<T>) {
        const MAX_ASYNC_ITER: usize = 1000;
        let mut iter = 0;
        loop {
            iter += 1;
            let idx = self.async_idx.load(SeqCst);
            if idx / 2 >= MAX_PENDING_ASYNC || iter > MAX_ASYNC_ITER {
                enqueuestats::drop_event();
                self.async_overflow.store(true, Ordering::SeqCst);
                log::debug!(
                    "dropped async trace event {:?} (overflow={}, timeout={})",
                    event,
                    idx / 2 >= MAX_PENDING_ASYNC,
                    iter > MAX_ASYNC_ITER
                );
                return;
            }

            if idx & 1 == 1 {
                enqueuestats::spin_busy();
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            if self
                .async_idx
                .compare_exchange(idx, idx + 1, SeqCst, SeqCst)
                .is_err()
            {
                enqueuestats::spin_cas();
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            unsafe {
                self.async_buffer
                    .get()
                    .cast::<(TraceEntryHead, BufferedTraceData)>()
                    .add(idx / 2)
                    .write(event.split_async());
            };
            self.async_idx.fetch_add(1, SeqCst);
            enqueuestats::event();
            if self.async_idx.load(SeqCst) / 2 >= ASYNC_SIGNAL_WATERMARK {
                self.signal_work();
            }
            return;
        }
    }

    pub fn drain_async(&self, mut f: impl FnMut(TraceEntryHead, BufferedTraceData)) {
        const MU: Option<(TraceEntryHead, BufferedTraceData)> = None;
        let mut buf = [MU; MAX_PENDING_ASYNC];
        loop {
            let idx = self.async_idx.load(SeqCst);
            if idx == 0 {
                return;
            }
            if idx & 1 == 1 {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            for i in 0..(idx / 2) {
                buf[i] = None;
                unsafe {
                    self.async_buffer
                        .get()
                        .cast::<Option<(TraceEntryHead, BufferedTraceData)>>()
                        .add(i)
                        .swap(&mut buf[i]);
                }
            }

            if self
                .async_idx
                .compare_exchange(idx, 0, SeqCst, SeqCst)
                .is_err()
            {
                crate::arch::processor::spin_wait_iteration();
                continue;
            }

            let overflow = self.async_overflow.swap(false, Ordering::SeqCst);
            log::debug!("drained {} async events (overflow={})", idx / 2, overflow);
            for i in 0..(idx / 2) {
                if let Some((mut h, d)) = buf[i].take() {
                    if i + 1 == idx / 2 && overflow {
                        h.flags.insert(TraceEntryFlags::DROPPED);
                    }
                    f(h, d);
                }
            }
            return;
        }
    }

    pub fn add_sink(&self, id: ObjID, spec: TraceSpec) -> Result<(), TwzError> {
        start_write_thread();
        let mut map = self.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        if let Some(sink) = map.get_mut(&id) {
            sink.modify(spec);
            drop(map);
        } else {
            drop(map);
            let sink = TraceSink::new(id, [spec].to_vec())?;
            let mut map = self.map.lock();

            if let Some(sink) = map.get_mut(&id) {
                sink.modify(spec);
            } else {
                map.insert(id, sink);
            }
            drop(map);
        }
        self.accum_all_events();
        self.signal_work();
        Ok(())
    }

    pub fn remove_sink(&self, id: ObjID) -> Result<(), TwzError> {
        let mut map = self.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        if let Some(sink) = map.get_mut(&id) {
            sink.write_all();
            // Explicitly, because `write_all` only announces on its watermark: this is the last
            // thing that will ever run for this sink, so anything still unannounced would be
            // collected data the consumer is never told about.
            sink.signal();
            map.remove(&id);
            drop(map);
            self.accum_all_events();
            Ok(())
        } else {
            Err(ObjectError::NoSuchObject.into())
        }
    }

    pub fn accum_all_events(&self) {
        let mut map = self.map.lock();
        let mut quicks = BTreeMap::<TraceKind, u64>::new();
        quicks.insert(TraceKind::Context, 0);
        quicks.insert(TraceKind::Kernel, 0);
        quicks.insert(TraceKind::Object, 0);
        quicks.insert(TraceKind::Pager, 0);
        quicks.insert(TraceKind::Security, 0);
        quicks.insert(TraceKind::Thread, 0);
        for sink in map.values_mut() {
            for spec in sink.specs() {
                let entry = quicks.entry(spec.kind).or_default();
                let events = spec.enable_events & !spec.disable_events;
                *entry |= events;
            }
        }
        for (k, e) in quicks {
            log::trace!("accum quick update: {:?}: {:x}", k, e);
            self.update_quick_enabled(k, e);
        }
    }
}

static KTRACE_THREAD: Once<u64> = Once::new();

pub fn is_thread_ktrace_thread(th: &ThreadRef) -> bool {
    KTRACE_THREAD.poll().is_some_and(|k| *k == th.id())
}

extern "C" fn kthread_trace_writer() {
    KTRACE_THREAD.call_once(|| current_thread_ref().unwrap().id());
    loop {
        let mut did_work = false;
        let mut map = TRACE_MGR.map.lock();
        TRACE_MGR.drain_async(|head, data| {
            did_work = true;
            for sink in map.values_mut() {
                if sink.accepts(&head) {
                    sink.enqueue((head, data.clone()));
                }
            }
        });
        for sink in map.values_mut() {
            if sink.write_all() {
                did_work = true;
            }
        }
        // About to go idle: announce whatever the watermark left unannounced. Every batch
        // therefore either has more work coming -- and is announced by a later pass -- or ends
        // here, so deferring the announcement cannot strand data.
        if !did_work {
            for sink in map.values_mut() {
                sink.signal();
            }
        }
        drop(map);
        let mut sig = TRACE_MGR.has_work.lock();
        log::trace!("ktrace thread: {} {}", did_work, *sig);
        if !*sig && !did_work {
            let _ = TRACE_MGR.cv.wait(sig);
        } else {
            *sig = false;
        }
    }
}

fn start_write_thread() {
    if current_thread_ref().is_some() {
        // TODO: dynamically adjust priority based on how many pending async events there are to
        // process.
        WRITE_THREAD.call_once(|| {
            start_new_kernel(
                Priority::BACKGROUND,
                kthread_trace_writer,
                0,
                "trace-writer",
            )
        });
    }
}
