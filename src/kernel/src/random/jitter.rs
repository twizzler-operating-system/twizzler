use core::sync::atomic::{AtomicU64, Ordering};

use rand_core::TryRngCore;
// see https://docs.rs/rand_jitter/0.4.0/rand_jitter/struct.JitterRng.html#example
use rand_jitter::JitterRng;

use super::{EntropySource, register_entropy_source};
use crate::time::TICK_SOURCES;

/// Largest gap `get_nstime` will report between two calls, ~16.8ms.
///
/// `rand_jitter`'s stuck-detector subtracts consecutive deltas as plain `i32`
/// (`EcState::stuck`), so one outlying gap overflows it and panics an overflow-checked build.
/// Two things produce such a gap here and neither is rare: contention on `TICK_SOURCES` below,
/// and an emulated host stalling the vcpu. The clock also reads a per-cpu `rdtsc`, so a migration
/// between calls can move it *backwards* -- which `wrapping_sub` then turns into a huge positive
/// delta. Bounding the reported gap fixes both without touching the crate: real jitter is
/// nanoseconds to microseconds, so this only discards outliers, which carry no entropy anyway.
const MAX_DELTA_NS: u64 = 1 << 24;

static PREV_RAW_NS: AtomicU64 = AtomicU64::new(0);
static ELAPSED_NS: AtomicU64 = AtomicU64::new(0);

pub fn get_nstime() -> u64 {
    let raw = {
        let ticks = { TICK_SOURCES.lock()[0].as_ref().unwrap().read() };
        let span = ticks.value * ticks.rate;
        span.as_nanos() as u64
    };

    let mut prev = PREV_RAW_NS.load(Ordering::Relaxed);
    let delta = loop {
        // saturating_sub, so a backwards clock reports no elapsed time rather than a huge one.
        let delta = raw.saturating_sub(prev).min(MAX_DELTA_NS);
        match PREV_RAW_NS.compare_exchange_weak(prev, raw, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break delta,
            Err(cur) => prev = cur,
        }
    };
    ELAPSED_NS.fetch_add(delta, Ordering::Relaxed) + delta
}

pub struct Jitter(JitterRng<fn() -> u64>);

impl EntropySource for Jitter {
    fn try_new() -> Result<Self, ()> {
        let mut jrng: JitterRng<fn() -> u64> = JitterRng::new_with_timer(get_nstime);
        let rounds = jrng.test_timer().or_else(|e| {
            logln!("Failed to instantiate Jitter: {}", e);
            Err(())
        })?;
        jrng.set_rounds(rounds);
        Ok(Jitter(jrng))
    }
    // shouldn't ever fail
    fn try_fill_entropy(&mut self, dest: &mut [u8]) -> Result<(), ()> {
        self.0.try_fill_bytes(dest).map_err(|_| ())
    }
}

pub fn maybe_add_jitter_entropy_source() -> bool {
    register_entropy_source::<Jitter>()
}
