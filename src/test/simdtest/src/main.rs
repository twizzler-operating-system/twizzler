//! Attempted targeted reproducer for the Mode L class: userspace SIMD registers not surviving a
//! context switch.
//!
//! **This does not work, and is deliberately not in the workspace.** It was run against a
//! known-bad kernel (the pre-fix `arch_switch_to`) at 64 KiB x 8 threads x 20 s and again at
//! 1 MiB x 8 threads x 30 s, and passed both times, while `memhog-test` on the same kernel
//! reproduces at roughly one run in five. Its fill loop vectorizes exactly like memhog's
//! (`ymm0`/`ymm1` identity, `ymm2` counter, `ymm10` mask -- checked in the disassembly), so the
//! shape is right and the exposure is not: the kernel's own probe measures only 8-32 stale
//! restores per suite run, so reproducing needs a fill long enough to contain one *and* the luck
//! to have the stale snapshot differ. Left here so the next person does not rebuild it from
//! scratch; wire it into the workspace members list to run it. If you tune it, validate against
//! a kernel with the fix reverted before believing a clean result.
//!
//! `memhog-test` finds this, but only via 256 MiB of memory traffic per round and at roughly one
//! sighting per six suite runs, which makes "the fix works" hard to demonstrate. The corruption
//! never needed the memory: what it needs is a hot loop whose *loop-invariant* values live in
//! vector registers, running long enough on more than one cpu to be migrated mid-loop.
//!
//! So: a small buffer, filled over and over with a per-generation record pattern, verified
//! immediately. If a fill's vector registers are replaced with a stale snapshot, the records it
//! writes carry an older generation's identity and counter -- which the record encoding names
//! outright. Several threads run this at once, because the race requires a second cpu.

use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

/// The corruption needs a cross-cpu pickup to land *inside* a fill, and those are rare -- the
/// kernel's own probe counts on the order of ten per suite run. So the fill has to be long enough
/// to contain one: 64 KiB was measured to be too short (a full run of it did not reproduce on a
/// known-bad kernel), while `memhog-test`'s 1 MiB does. The leverage over memhog is thread count,
/// not buffer size -- memhog fills one buffer at a time on one thread.
const BUF_BYTES: usize = 1 << 20;
const RECORD: usize = 16;
const TAG: u64 = 0x5344; // "SD"
const SECONDS: u64 = 30;

static MISMATCHES: AtomicU64 = AtomicU64::new(0);
static FILLS: AtomicU64 = AtomicU64::new(0);

/// `[tag:16][thread:8][generation:16][block:24]`. Generation is loop-invariant across one fill, so
/// it lives in a vector register alongside the tag; block is the running counter in another.
fn record_word(thread: usize, generation: usize, block: usize) -> u64 {
    (TAG << 48)
        | ((thread as u64 & 0xff) << 40)
        | ((generation as u64 & 0xffff) << 24)
        | (block as u64 & 0xff_ffff)
}

fn fill(buf: &mut [u8], thread: usize, generation: usize) {
    for (block, rec) in buf.chunks_exact_mut(RECORD).enumerate() {
        let w = record_word(thread, generation, block);
        rec[0..8].copy_from_slice(&w.to_le_bytes());
        rec[8..16].copy_from_slice(&(!w).to_le_bytes());
    }
}

fn check(buf: &[u8], thread: usize, generation: usize) -> Option<(usize, u64)> {
    for (block, rec) in buf.chunks_exact(RECORD).enumerate() {
        let w = record_word(thread, generation, block);
        if rec[0..8] != w.to_le_bytes() || rec[8..16] != (!w).to_le_bytes() {
            return Some((block, u64::from_le_bytes(rec[0..8].try_into().unwrap())));
        }
    }
    None
}

/// Decode whatever was found there. A stale-register fault names a real, earlier generation of
/// *this* thread; anything else is a different bug and should not be reported as this one.
fn describe(observed: u64, thread: usize, generation: usize, block: usize) -> String {
    if observed >> 48 != TAG {
        return format!("not one of our records ({observed:#x})");
    }
    let t = ((observed >> 40) & 0xff) as usize;
    let g = ((observed >> 24) & 0xffff) as usize;
    let b = (observed & 0xff_ffff) as usize;
    format!(
        "record says thread {t} gen {g} block {b} (ours: thread {thread} gen {generation} \
         block {block}){}",
        if t == thread && g != generation {
            " => STALE VECTOR STATE FROM AN EARLIER GENERATION"
        } else if t != thread {
            " => another thread's identity"
        } else {
            " => same generation, shifted block"
        }
    )
}

fn worker(thread: usize, deadline: Instant) {
    let mut buf = vec![0u8; BUF_BYTES];
    let mut generation = 0usize;
    while Instant::now() < deadline {
        for _ in 0..64 {
            generation = generation.wrapping_add(1) & 0xffff;
            fill(&mut buf, thread, generation);
            FILLS.fetch_add(1, Ordering::Relaxed);
            if let Some((block, observed)) = check(&buf, thread, generation) {
                MISMATCHES.fetch_add(1, Ordering::Relaxed);
                println!(
                    "simdtest: MISMATCH thread {thread} gen {generation} at block {block}: {}",
                    describe(observed, thread, generation, block)
                );
            }
        }
    }
}

#[cfg_attr(test, test)]
fn simd_state_survives_context_switch() {
    // Oversubscribe: the race needs a thread to be queued onto another cpu while it is still
    // running here, which takes more runnable threads than cpus.
    let nthreads = std::thread::available_parallelism().map_or(8, |n| n.get().max(2) * 4);
    println!("simdtest: {nthreads} threads for {SECONDS}s, buffer {BUF_BYTES} bytes");
    let deadline = Instant::now() + std::time::Duration::from_secs(SECONDS);
    let handles: Vec<_> = (0..nthreads)
        .map(|t| std::thread::spawn(move || worker(t, deadline)))
        .collect();
    for h in handles {
        let _ = h.join();
    }
    let (bad, fills) = (
        MISMATCHES.load(Ordering::Relaxed),
        FILLS.load(Ordering::Relaxed),
    );
    println!("simdtest: {fills} fills, {bad} mismatches");
    assert_eq!(bad, 0, "vector register state was not preserved across a context switch");
}

#[cfg(not(test))]
fn main() {
    simd_state_survives_context_switch();
    println!("simdtest: ok");
}
