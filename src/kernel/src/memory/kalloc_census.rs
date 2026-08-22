//! Kernel-heap allocation census by size class.
//!
//! `mem.kalloc_bytes` is a net-live counter -- every `alloc` adds its layout size and every
//! `dealloc` subtracts it -- so a sustained slope on it is bytes the kernel allocated and never
//! freed. It says how many; it does not say which. This records gross alloc/free counts and bytes
//! per size class, so a per-operation diff names the class, which is usually enough to name the
//! struct.
//!
//! Gross rather than net per bucket on purpose: a class with a small net and heavy churn and a
//! class allocated twice and never freed produce the same net and want different investigations.

use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use twizzler_abi::syscall::{KALLOC_NR_BUCKETS, KallocCensus};

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: AtomicU64 = AtomicU64::new(0);

static ALLOC_COUNT: [AtomicU64; KALLOC_NR_BUCKETS] = [ZERO; KALLOC_NR_BUCKETS];
static FREE_COUNT: [AtomicU64; KALLOC_NR_BUCKETS] = [ZERO; KALLOC_NR_BUCKETS];
static ALLOC_BYTES: [AtomicU64; KALLOC_NR_BUCKETS] = [ZERO; KALLOC_NR_BUCKETS];
static FREE_BYTES: [AtomicU64; KALLOC_NR_BUCKETS] = [ZERO; KALLOC_NR_BUCKETS];

/// 16-byte granularity below 1 KiB, powers of two above. Small classes are where a per-object
/// leak lands and where resolution matters; large ones only need to be distinguishable.
#[inline]
fn bucket(size: usize) -> usize {
    if size < 1024 {
        size / 16
    } else {
        let log = usize::BITS as usize - 1 - size.leading_zeros() as usize;
        (64 + log).min(KALLOC_NR_BUCKETS - 1)
    }
}

/// Off unless `--kalloc-census` is passed.
///
/// The hooks sit on every kernel heap alloc and free, so an unflagged boot must pay a relaxed load
/// of a shared-read line and nothing else: two contended `fetch_add`s on that path measurably
/// perturb allocation-heavy workloads, which is a boundary across every other measurement in this
/// tree. Runtime rather than a `const` on purpose -- a const forks the tree state, and an A/B whose
/// arms are different source trees is not an A/B.
static CENSUS_ON: AtomicBool = AtomicBool::new(false);

pub fn enable() {
    CENSUS_ON.store(true, Ordering::Relaxed);
    logln!("[kalloc-census] enabled");
}

#[inline]
pub fn enabled() -> bool {
    CENSUS_ON.load(Ordering::Relaxed)
}

#[inline]
pub fn record_alloc(size: usize) {
    if !enabled() {
        return;
    }
    let b = bucket(size);
    ALLOC_COUNT[b].fetch_add(1, Ordering::Relaxed);
    ALLOC_BYTES[b].fetch_add(size as u64, Ordering::Relaxed);
    maybe_trap(size);
}

#[inline]
pub fn record_free(size: usize) {
    if !enabled() {
        return;
    }
    let b = bucket(size);
    FREE_COUNT[b].fetch_add(1, Ordering::Relaxed);
    FREE_BYTES[b].fetch_add(size as u64, Ordering::Relaxed);
}

pub fn fill(census: &mut KallocCensus) {
    for b in 0..KALLOC_NR_BUCKETS {
        census.buckets[b].alloc_count = ALLOC_COUNT[b].load(Ordering::Relaxed);
        census.buckets[b].free_count = FREE_COUNT[b].load(Ordering::Relaxed);
        census.buckets[b].alloc_bytes = ALLOC_BYTES[b].load(Ordering::Relaxed);
        census.buckets[b].free_bytes = FREE_BYTES[b].load(Ordering::Relaxed);
    }
}

/// A size-class trap: print a kernel backtrace for allocations whose size falls in `[lo, hi]`.
///
/// The census says which class retains bytes; a class is not a call site. This turns one into the
/// other, at the cost of a few symbolized backtraces. Bounded by `TRAP_LEFT` because a backtrace
/// from inside the global allocator is expensive and one per allocation would change the workload
/// it is measuring.
static TRAP_LO: AtomicUsize = AtomicUsize::new(usize::MAX);
static TRAP_HI: AtomicUsize = AtomicUsize::new(0);
static TRAP_EVERY: AtomicU64 = AtomicU64::new(1);
static TRAP_SEEN: AtomicU64 = AtomicU64::new(0);
static TRAP_LEFT: AtomicU64 = AtomicU64::new(0);
/// Backtracing allocates. Without this the first trap recurses until the stack runs out.
static TRAP_BUSY: AtomicBool = AtomicBool::new(false);

/// `--kalloc-trap=<lo>:<hi>[:<every>[:<max>]]`
///
/// **Disarmed 2026-08-20, deliberately, and this is an interlock rather than a removal.**
/// [`maybe_trap`] calls `panic::backtrace(true, ..)` from inside `GlobalAlloc::alloc`:
/// symbolization parses DWARF, which allocates, re-entering the allocation path and
/// `GLOBAL_PAGE_ALLOC`, and it writes through the console lock -- with whatever locks the caller
/// happens to hold. `TRAP_BUSY` guards only against re-entering the trap itself. leakcheck.md has
/// said "must not be used again until rewritten" since it was withdrawn; a comment saying so is
/// not an interlock, and the next person to reach for it will be someone chasing a leak at 3am.
///
/// It is also superseded. `--track`
/// ([kalloc_track.rs](../kalloc_track.rs)) answers the question this was reached for and answers it
/// better: it records the *live set* with raw return addresses, so it can isolate the blocks that
/// were never freed rather than sampling callers of which almost all did free -- and it never
/// symbolizes, prints, or allocates inside the allocation path.
///
/// Re-arming means fixing `maybe_trap` first, at which point this wrapper comes out with it. The
/// original body is preserved verbatim below rather than deleted -- it is someone else's
/// uncommitted work, and the parsing is not the broken part.
pub fn set_trap(_arg: &str) {
    logln!(
        "[kalloc-census] --kalloc-trap is disabled: it symbolizes from inside \
         GlobalAlloc::alloc, which allocates and can self-deadlock. Use --track / \
         sys_kalloc_track instead."
    );
}

#[allow(dead_code)]
fn set_trap_inner(arg: &str) {
    let mut it = arg.split(':');
    let lo = it.next().and_then(|s| s.parse::<usize>().ok());
    let hi = it.next().and_then(|s| s.parse::<usize>().ok());
    let (Some(lo), Some(hi)) = (lo, hi) else {
        logln!("[kalloc-census] malformed --kalloc-trap `{}'", arg);
        return;
    };
    let every = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);
    let max = it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(8);
    TRAP_LO.store(lo, Ordering::SeqCst);
    TRAP_HI.store(hi, Ordering::SeqCst);
    TRAP_EVERY.store(every.max(1), Ordering::SeqCst);
    TRAP_LEFT.store(max, Ordering::SeqCst);
    logln!(
        "[kalloc-census] trapping allocations of {}..={} bytes, every {}, {} times",
        lo,
        hi,
        every,
        max
    );
}

#[inline]
fn maybe_trap(size: usize) {
    if size < TRAP_LO.load(Ordering::Relaxed) || size > TRAP_HI.load(Ordering::Relaxed) {
        return;
    }
    let n = TRAP_SEEN.fetch_add(1, Ordering::Relaxed);
    if n % TRAP_EVERY.load(Ordering::Relaxed) != 0 {
        return;
    }
    if TRAP_LEFT.load(Ordering::Relaxed) == 0 {
        return;
    }
    if TRAP_BUSY.swap(true, Ordering::SeqCst) {
        return;
    }
    if TRAP_LEFT.fetch_sub(1, Ordering::SeqCst) > 0 {
        emerglogln!("[kalloc-census] allocation of {} bytes (#{}):", size, n);
        crate::panic::backtrace(true, None);
    }
    TRAP_BUSY.store(false, Ordering::SeqCst);
}
