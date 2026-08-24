//! The monitor's own heap, per size class.
//!
//! `leakcheck`'s object census can say that the object noted `monitor-heap` gains ~1 page per
//! compartment load and keeps doing it across 1100 loads without decaying. It cannot say what is
//! in those pages, because the object is talc's arena and the arena is one allocation from the
//! kernel's point of view.
//!
//! The reference runtime already carries a per-size-class census (`runtime/alloc.rs::census`),
//! inert until armed. Its statics live in the *calling compartment's* copy of `libtwz_rt.so`'s
//! data segment, so arming it in `leakcheck` measures `leakcheck`; reaching the monitor's heap
//! means arming it here. The monitor allocates through that runtime like any other compartment --
//! `main.rs`'s `#[global_allocator]` is commented out precisely because std's default allocator on
//! this target already is the reference runtime.
//!
//! Absolute counters, not deltas: two consecutive snapshots subtract offline, and a snapshot that
//! is wrong is visible as a discontinuity rather than folded into a rate.

use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use twizzler_abi::klog_println;

/// Snapshot every Nth call. `leakcheck` calls `monitor_rt_stats` about once per iteration.
///
/// Was 64, which is ~3 snapshots per 220-iteration op -- enough to bracket an op, and *not* enough
/// to catch the le=128 drain that happens just after each op's cleanup. Reading only the peaks that
/// survive that sampling is what made a draining working set look like ~90 blocks held forever.
/// At 16 each op is sampled ~14 times, so the floor between ops is measured rather than inferred.
const PERIOD: u64 = 64;

const NR_BRANCH: usize = 16;
const NR_CLASSES: usize = 32;
const NR_WORDS: usize = NR_BRANCH + NR_CLASSES * 4;

/// Same order as `runtime/alloc.rs::census`'s `B_*` constants. Counts in `w[i]`, bytes in `w[i+8]`.
const BRANCH: [&str; 8] = [
    "ferroc",
    "early_cold",
    "early_nots",
    "talc",
    "drop_notready",
    "drop_earlyptr",
    "drop_nulltls",
    "drop_nots",
];

extern "C-unwind" {
    fn __twz_rt_diag_heap_census(out: *mut u64, n: usize) -> usize;
    fn __twz_rt_diag_heap_census_arm() -> u64;
}

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Arm on the first call, snapshot every [`PERIOD`]th.
///
/// Must be called with no monitor lock held: it prints, and a print is a syscall.
pub fn tick() {
    let n = SEQ.fetch_add(1, Relaxed);
    if n == 0 {
        // Reports the prior state so that "armed by us" and "already armed by something else" are
        // distinguishable -- two arms in one boot must not both believe they own the window.
        let was = unsafe { __twz_rt_diag_heap_census_arm() };
        klog_println!("MONHEAP-ARM was_armed={}", was);
        positive_control();
        track_arm();
        track_positive_control();
        return;
    }
    if n % PERIOD != 0 {
        return;
    }
    snapshot(n);
}

/// One allocation of a size nothing else here uses, made and freed immediately after arming.
///
/// An all-zero census reads the same whether the monitor allocated nothing or the hooks never see
/// the monitor's allocations at all -- the monitor's `#[global_allocator]` is commented out on the
/// premise that std's default allocator on this target *is* the reference runtime, and that premise
/// is exactly what would make the instrument silently blind. The first snapshot must show
/// `le=131072` with `alloc>=1` and `free>=1`; if it does not, read nothing else in the table.
const CONTROL_SIZE: usize = 1 << 17;

fn positive_control() {
    let mut v: Vec<u8> = Vec::with_capacity(CONTROL_SIZE);
    // Push, then escape the pointer. `with_capacity` + `black_box(v.capacity())` is elided
    // outright by LLVM -- three ops in `leakcheck` compile to no `__rust_alloc` for exactly that
    // reason. This is the form `l2ctl-48k`/`l2ctl-48b` use, which is known to survive.
    v.push(0xa5);
    core::hint::black_box(v.as_ptr());
    drop(v);
    klog_println!("MONHEAP-CONTROL alloc_and_free_bytes={}", CONTROL_SIZE);
}

/// A second control, inside the *tracker's* window.
///
/// `CONTROL_SIZE` is 131072, which is outside `[TRACK_LO, TRACK_HI]` -- so it exercises the census
/// and says nothing about the tracker, whose `live=0` then reads the same whether its hook fires or
/// not. Caught by twizzler-d3 after a run had already been read that way. Must be called *after*
/// `track_arm`. 121 is odd and not a power of two, so it cannot be mistaken for a real population
/// in a window (64, 128] where real allocations cluster on round sizes.
const TRACK_CONTROL_SIZE: usize = 121;

fn track_positive_control() {
    let mut v: Vec<u8> = Vec::with_capacity(TRACK_CONTROL_SIZE);
    v.push(0xa5);
    core::hint::black_box(v.as_ptr());
    drop(v);
    klog_println!(
        "MONHEAP-TRACK-CONTROL alloc_and_free_bytes={}",
        TRACK_CONTROL_SIZE
    );
}

fn snapshot(seq: u64) {
    let mut w = [0u64; NR_WORDS];
    // Stack, not heap: allocating here would be counted by the census it is reading.
    let got = unsafe { __twz_rt_diag_heap_census(w.as_mut_ptr(), NR_WORDS) };
    if got != NR_WORDS {
        // An unarmed census returns 0 and leaves the buffer zeroed, which is the same table a
        // monitor that allocated nothing would produce.
        klog_println!("MONHEAP seq={} unavailable got={}", seq, got);
        return;
    }

    for (i, name) in BRANCH.iter().enumerate() {
        if w[i] == 0 && w[i + 8] == 0 {
            continue;
        }
        klog_println!("MONHEAP-BRANCH seq={} {}={}/{}", seq, name, w[i], w[i + 8]);
    }

    let mut live_bytes: i64 = 0;
    for c in 0..NR_CLASSES {
        let b = NR_BRANCH + c * 4;
        let (ac, ab, fc, fb) = (w[b], w[b + 1], w[b + 2], w[b + 3]);
        if ac == 0 && fc == 0 {
            continue;
        }
        live_bytes += ab as i64 - fb as i64;
        klog_println!(
            "MONHEAP-CLASS seq={} le={} alloc={} allocb={} free={} freeb={} net={} netb={}",
            seq,
            1u64 << c,
            ac,
            ab,
            fc,
            fb,
            ac as i64 - fc as i64,
            ab as i64 - fb as i64
        );
    }
    klog_println!("MONHEAP seq={} live_bytes={}", seq, live_bytes);
    track_report(seq);
}

// ---- live-block sizes in one class ------------------------------------------------------------
//
// The census names a size *class*; `le=128` covers (64, 128]. A histogram of the exact sizes
// inside it says whether the retained population is one repeated allocation -- which is a
// greppable fingerprint -- or a mixture, which is not.
//
// Sizes only. The runtime's tracker also hands back pointers, and `leakcheck` dereferences them to
// print each block's first bytes; doing that *here* would fault the monitor if a block were freed
// between dump and read, and a fault in the monitor takes the system. The size is enough to
// identify a call site by grep.

use core::sync::atomic::AtomicBool;

const TRACK_LO: usize = 65;
const TRACK_HI: usize = 128;
/// (N - 5) / 2 blocks. Static rather than stack: 16 KiB is far past a comfortable frame, and
/// allocating it would be counted by the census it is reporting on.
const TRACK_WORDS: usize = 2048;

static TRACK_BUSY: AtomicBool = AtomicBool::new(false);
static mut TRACK_BUF: [u64; TRACK_WORDS] = [0; TRACK_WORDS];

extern "C-unwind" {
    fn __twz_rt_diag_heap_track_arm(lo: usize, hi: usize);
    fn __twz_rt_diag_heap_track_dump(out: *mut u64, n: usize) -> usize;
}

fn track_arm() {
    unsafe { __twz_rt_diag_heap_track_arm(TRACK_LO, TRACK_HI) };
    klog_println!("MONHEAP-TRACK-ARM lo={} hi={}", TRACK_LO, TRACK_HI);
}

/// Histogram of live block sizes in `[TRACK_LO, TRACK_HI]`, biggest population first.
fn track_report(seq: u64) {
    if TRACK_BUSY.swap(true, Relaxed) {
        // Another thread is mid-dump. Skipping is correct; two writers into one buffer is not.
        return;
    }
    let n = unsafe { __twz_rt_diag_heap_track_dump(TRACK_BUF.as_mut_ptr(), TRACK_WORDS) };
    if n < 5 {
        klog_println!("MONHEAP-TRACK seq={} unavailable n={}", seq, n);
        TRACK_BUSY.store(false, Relaxed);
        return;
    }
    let pairs = (n - 5) / 2;
    // Counting sort by distinct size, linear scan -- the population is small and this allocates
    // nothing. 32 distinct sizes is far more than any real call-site mix in one class.
    let mut size = [0u64; 32];
    let mut count = [0u64; 32];
    let mut distinct = 0usize;
    let mut overflow = 0u64;
    for i in 0..pairs {
        let sz = unsafe { TRACK_BUF[i * 2 + 1] };
        match (0..distinct).find(|&j| size[j] == sz) {
            Some(j) => count[j] += 1,
            None if distinct < 32 => {
                size[distinct] = sz;
                count[distinct] = 1;
                distinct += 1;
            }
            None => overflow += 1,
        }
    }
    let (inserted, removed, ovf, trunc) = unsafe {
        (
            TRACK_BUF[pairs * 2],
            TRACK_BUF[pairs * 2 + 1],
            TRACK_BUF[pairs * 2 + 2],
            TRACK_BUF[pairs * 2 + 4],
        )
    };
    klog_println!(
        "MONHEAP-TRACK seq={} live={} distinct={} unbinned={} inserted={} removed={} slot_overflow={} truncated={}",
        seq, pairs, distinct, overflow, inserted, removed, ovf, trunc
    );
    for j in 0..distinct {
        klog_println!(
            "MONHEAP-TRACK-SIZE seq={} bytes={} live={}",
            seq,
            size[j],
            count[j]
        );
    }
    TRACK_BUSY.store(false, Relaxed);
}
