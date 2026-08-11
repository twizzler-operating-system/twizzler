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
    reported: Option<Instant>,
}

static ACTIVE: Mutex<BTreeMap<u64, Entry>> = Mutex::new(BTreeMap::new());
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

/// A registered unit of work. Deregisters on drop, so an early return cannot leave a phantom entry
/// behind that the sampler would report forever.
pub struct Work {
    id: u64,
}

impl Work {
    /// Record that this work item has reached `phase`, which the sampler prints if it wedges here.
    pub fn phase(&self, phase: &'static str) {
        with_active(|active| {
            if let Some(entry) = active.get_mut(&self.id) {
                entry.phase = phase;
            }
        });
    }
}

impl Drop for Work {
    fn drop(&mut self) {
        let finished = with_active(|active| active.remove(&self.id));
        // Only interesting if the sampler already complained about it: that distinguishes work that
        // was merely slow from work that never came back.
        if let Some(entry) = finished {
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

fn sampler_main() {
    loop {
        std::thread::sleep(SAMPLE_EVERY);
        let now = Instant::now();

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
