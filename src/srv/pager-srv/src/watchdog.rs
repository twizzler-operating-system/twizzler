//! Progress tracking for in-flight kernel requests.
//!
//! When the pager stops answering, the kernel can only report that a request never came back. It
//! cannot say which pager thread owns that request, nor where that thread stopped, and a wedged
//! pager produces no other output at all -- so the two are indistinguishable from the transcript.
//!
//! Every unit of request-handling work registers a [`Work`] guard and stamps a phase as it crosses
//! each await, and a dedicated thread samples them. The sampler is its own thread (not a task on a
//! worker's executor, which a blocked worker would stop polling) and never blocks on a slot, so a
//! wedged thread cannot suppress the report. When anything is stuck it dumps *every* slot in one
//! burst: whether both workers are parked in the same phase is the thing worth knowing.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use twizzler_abi::pager::RequestFromKernel;

/// Report a work item that has been outstanding this long. Just under the kernel's own 2s inflight
/// timeout (`Request::is_timed_out`), so the pager names the culprit before the kernel starts
/// re-reporting the request forever.
const STUCK_AFTER: Duration = Duration::from_millis(1500);
/// Re-report a still-stuck item on this interval, rather than per sample.
const REPEAT_EVERY: Duration = Duration::from_secs(15);
const SAMPLE_EVERY: Duration = Duration::from_millis(250);
/// Cap on lines per burst. Prefetch can leave dozens of page-data tasks in flight, and a report
/// that drowns the transcript is the failure mode this diagnostic is supposed to avoid.
const MAX_REPORTED: usize = 16;

struct Entry {
    owner: &'static str,
    qid: u32,
    req: RequestFromKernel,
    start: Instant,
    phase: &'static str,
    /// When the current phase began, so each phase can be charged its own duration.
    phase_start: Instant,
    reported: Option<Instant>,
}

static ACTIVE: Mutex<BTreeMap<u64, Entry>> = Mutex::new(BTreeMap::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// Per-phase time, keyed by the phase name a work item was *leaving*.
///
/// The phase markers already bracket every store call on the create, delete and sync paths, so
/// timing them costs one `Instant::now()` per marker and needs no new instrumentation sites. This
/// is deliberately separate from the watchdog's stall reporting: that fires only when something
/// wedges, and says nothing about where a request that completes normally spent its time.
static PHASE_STATS: Mutex<BTreeMap<&'static str, PhaseAcc>> = Mutex::new(BTreeMap::new());

#[derive(Default, Clone, Copy)]
struct PhaseAcc {
    n: u64,
    sum_ns: u64,
    max_ns: u64,
    /// Watermarks for the delta report; see [`phase_delta_report`].
    last_n: u64,
    last_sum_ns: u64,
}

fn record_phase(phase: &'static str, ns: u64) {
    let mut stats = PHASE_STATS.lock().unwrap_or_else(|e| e.into_inner());
    let acc = stats.entry(phase).or_default();
    acc.n += 1;
    acc.sum_ns += ns;
    acc.max_ns = acc.max_ns.max(ns);
}

/// Phase time accrued since the last call, omitting phases that did not move.
///
/// Deltas rather than cumulative totals, for two reasons. Attribution: consecutive ticks bracket
/// whatever ran between them, so a tick pair spanning one `SYSBENCH-MARK` charges that bench.
/// Cost: an idle interval prints nothing and a busy one prints only the handful of phases actually
/// running, instead of ~18 every time. A console line costs on the order of a millisecond of
/// emulated UART, so a chatty diagnostic inside a measured window inflates the number it exists to
/// explain.
pub fn phase_delta_report() -> Option<String> {
    let mut stats = PHASE_STATS.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<(&'static str, u64, u64, u64)> = Vec::new();
    for (name, acc) in stats.iter_mut() {
        let dn = acc.n - acc.last_n;
        let dsum = acc.sum_ns - acc.last_sum_ns;
        if dn == 0 {
            continue;
        }
        acc.last_n = acc.n;
        acc.last_sum_ns = acc.sum_ns;
        rows.push((name, dn, dsum, acc.max_ns));
    }
    if rows.is_empty() {
        return None;
    }
    rows.sort_by_key(|r| core::cmp::Reverse(r.2));
    Some(
        rows.iter()
            .map(|(name, dn, dsum, max)| {
                format!(
                    "{} n={} total={}us mean={}us maxever={}us",
                    name,
                    dn,
                    dsum / 1000,
                    dsum / (*dn).max(1) / 1000,
                    max / 1000,
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Per-phase totals, ordered by total time spent, which is the order that answers "where did the
/// request go". Mean alone hides a phase that is cheap per call and runs on every request.
pub fn phase_report() -> String {
    let stats = PHASE_STATS.lock().unwrap_or_else(|e| e.into_inner());
    let mut rows: Vec<_> = stats.iter().map(|(k, v)| (*k, *v)).collect();
    rows.sort_by_key(|(_, v)| core::cmp::Reverse(v.sum_ns));
    if rows.is_empty() {
        return "none".to_string();
    }
    rows.iter()
        .map(|(name, acc)| {
            format!(
                "{} n={} total={}us mean={}us max={}us",
                name,
                acc.n,
                acc.sum_ns / 1000,
                acc.sum_ns / acc.n.max(1) / 1000,
                acc.max_ns / 1000,
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// A registered unit of work. Deregisters on drop, so an early return cannot leave a phantom entry
/// behind that the sampler would report forever.
pub struct Work {
    id: u64,
}

impl Work {
    /// Record that this work item has reached `phase`, which the sampler prints if it wedges here,
    /// and charge the elapsed time to the phase being left.
    ///
    /// `PHASE_STATS` is taken *after* `ACTIVE` is released rather than nested inside it: the two
    /// locks are then never held together, so no ordering discipline is needed between them.
    pub fn phase(&self, phase: &'static str) {
        let now = Instant::now();
        let left = with_active(|active| {
            active.get_mut(&self.id).map(|entry| {
                let prev = entry.phase;
                let elapsed = now.saturating_duration_since(entry.phase_start);
                entry.phase = phase;
                entry.phase_start = now;
                (prev, elapsed)
            })
        });
        if let Some((prev, elapsed)) = left {
            record_phase(prev, elapsed.as_nanos() as u64);
        }
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let finished = with_active(|active| active.remove(&self.id));
        // Only interesting if the sampler already complained about it: that distinguishes work that
        // was merely slow from work that never came back.
        if let Some(entry) = finished {
            // The last phase has no successor marker to close it, so it is charged here; without
            // this every request's final phase would be missing from the totals.
            record_phase(entry.phase, entry.phase_start.elapsed().as_nanos() as u64);
            if entry.reported.is_some() {
                tracing::warn!(
                    "pager watchdog: {} finished qid {} after {}ms in phase '{}': {:?}",
                    entry.owner,
                    entry.qid,
                    entry.start.elapsed().as_millis(),
                    entry.phase,
                    entry.req,
                );
            }
        }
    }
}

/// Register a unit of request-handling work owned by `owner` (a thread name, or the name of a
/// detached task and the thread whose executor polls it).
pub fn begin(owner: &'static str, qid: u32, req: RequestFromKernel) -> Work {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    with_active(|active| {
        active.insert(
            id,
            Entry {
                owner,
                qid,
                req,
                start: Instant::now(),
                phase: "start",
                phase_start: Instant::now(),
                reported: None,
            },
        );
    });
    Work { id }
}

/// The registry is only ever held across a map operation, never across an await or anything that
/// can panic, so poisoning is unreachable -- but recovering beats taking the pager down over the
/// diagnostic that exists to explain why it stopped.
fn with_active<R>(f: impl FnOnce(&mut BTreeMap<u64, Entry>) -> R) -> R {
    let mut active = ACTIVE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut active)
}

pub fn start() {
    let _ = std::thread::Builder::new()
        .name("pager-watchdog".to_owned())
        .spawn(sampler_main);
}

/// Heartbeat interval for the unconditional `NVMEQ` dump. See `NvmeController::queue_diag` for why
/// it is not folded into the stall report.
const QUEUE_DIAG_EVERY: Duration = Duration::from_secs(20);

/// Cadence for the cumulative phase dump.
///
/// `DispatchStats::report` also prints one, but it fires on powers of two of the transit count, so
/// late in a boot -- which is when the benchmarks run -- reports are thousands of requests apart
/// and cannot be aligned to any single bench. A fixed interval gives a series whose consecutive
/// differences attribute time to whatever ran between them, against the `SYSBENCH-MARK` lines in
/// the same log.
const PHASE_REPORT_EVERY: Duration = Duration::from_secs(2);

fn sampler_main() {
    let mut last_diag = Instant::now();
    let mut last_phase = Instant::now();
    loop {
        std::thread::sleep(SAMPLE_EVERY);
        let now = Instant::now();
        if now.duration_since(last_diag) >= QUEUE_DIAG_EVERY {
            last_diag = now;
            crate::nvme::queue_diag();
        }
        // Before the `try_lock`/`due` early-outs below: those skip most rounds, and a dump that
        // only appeared when something was already stuck would be missing for every healthy boot,
        // which is exactly the population being measured.
        if now.duration_since(last_phase) >= PHASE_REPORT_EVERY {
            last_phase = now;
            if let Some(delta) = phase_delta_report() {
                tracing::info!("PHASETICK: {}", delta);
            }
        }

        // try_lock, not lock: if some future change ever holds the registry longer, the sampler
        // skips a round rather than joining the pile-up it is supposed to describe.
        let Ok(mut active) = ACTIVE.try_lock() else {
            continue;
        };

        let due = active.values().any(|entry| {
            now.duration_since(entry.start) >= STUCK_AFTER
                && entry
                    .reported
                    .is_none_or(|at| now.duration_since(at) >= REPEAT_EVERY)
        });
        if !due {
            continue;
        }

        // Keyed by a monotonic id, so this iterates oldest-first and the cap drops the youngest --
        // a burst of fresh page-data tasks must not push the wedged item out of the report.
        let total = active.len();
        tracing::warn!(
            "pager watchdog: {} work item(s) in flight{}",
            total,
            if total > MAX_REPORTED {
                ", oldest first"
            } else {
                ""
            }
        );
        for (n, entry) in active.values_mut().enumerate() {
            let age = now.duration_since(entry.start);
            if n < MAX_REPORTED {
                tracing::warn!(
                    "pager watchdog:   {} qid {} age {}ms phase '{}': {:?}",
                    entry.owner,
                    entry.qid,
                    age.as_millis(),
                    entry.phase,
                    entry.req,
                );
            }
            // Stamped even when it did not fit in the report, or an entry past the cap keeps the
            // burst due on every sample.
            if age >= STUCK_AFTER {
                entry.reported = Some(now);
            }
        }

        // Every stuck phase seen so far sits below the object store, waiting on disk. Dump the
        // controller in the same burst so the transcript says whether a completion is sitting
        // unconsumed (our bug) or was never posted (the command's).
        crate::nvme::dump_stall();
    }
}
