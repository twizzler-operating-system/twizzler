//! Driving the system's deferred reclamation to completion, and reporting whether it got there.
//!
//! Sleeping is not enough. The reference runtime's handle cache holds a released handle for
//! `IDLE_TTL = 2s`, but that expiry is only enforced when someone touches the cache -- a sleeping
//! process never triggers it, and until the entry goes the kernel cannot tell the mapping from a
//! live one, so `scan_deleted` will not reap the object behind it. So: poke, then wait, then check.
//!
//! Waiting also needs an idle cpu. `scan_deleted` runs from the bsp idle loop every 1000
//! iterations and `cleanup_exited` every 100 (popping a single thread per call) -- on an smp1 boot
//! nothing idles while anything runs, and neither ever fires. Run at -smp 4.
//!
//! `twz_rt_gc` reaches only the calling compartment's own caches. The monitor's unmapper, the
//! monitor's thread cleaner and the pager's deleter run on their own schedules and can only be
//! waited for, which is what the convergence loop is.

use std::time::{Duration, Instant};

use crate::sample::Sample;

pub struct Quiesced {
    /// Two consecutive samples agreed before the budget ran out.
    pub converged: bool,
    pub elapsed_ms: u64,
}

const POLL: Duration = Duration::from_millis(250);

/// Poke every cache we can reach, then sample until two consecutive samples are identical or
/// `budget_ms` expires.
///
/// A non-convergence is a result, not a failure: it means something is still moving after seconds
/// of idle, and the caller reports it rather than quietly accepting the last sample.
pub fn quiesce(budget_ms: u64) -> Quiesced {
    let start = Instant::now();
    let budget = Duration::from_millis(budget_ms);
    let mut last: Option<Sample> = None;

    loop {
        // Poke first, every round: gc_threads + heap_gc + gc_object_cache. Once is not enough --
        // dropping a handle can queue an unmap whose completion frees the next thing.
        twizzler_rt_abi::core::twz_rt_gc();
        std::thread::sleep(POLL);

        let s = Sample::take();
        let stable = last.as_ref().is_some_and(|l| l.settled_eq(&s));
        last = Some(s);

        if stable {
            return Quiesced {
                converged: true,
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
        if start.elapsed() >= budget {
            return Quiesced {
                converged: false,
                elapsed_ms: start.elapsed().as_millis() as u64,
            };
        }
    }
}
