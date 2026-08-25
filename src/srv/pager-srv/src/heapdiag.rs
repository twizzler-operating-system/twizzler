//! pager-srv's own heap, per size class.
//!
//! The runtime's census (`runtime/alloc.rs::census`) is inert until armed, and its statics live in
//! the *calling compartment's* copy of `libtwz_rt.so`'s data segment -- so arming it in
//! `naming-srv` measures naming and reaching pager-srv's heap means arming it here. Until now
//! pager-srv was the one service with no census, which is why its `+128`-page step has only ever
//! been attributed by an object note -- and that note is written from the allocator's OOM handler
//! using `get_sctx_id()`, which during a gate call is the *callee's* context, not the allocating
//! compartment's.
//!
//! Absolute counters alongside per-window deltas: a block allocated in window N and freed in N+1
//! shows as +1 then -1, so a single window over-reports retention and only the running total
//! attributes the run.

use std::sync::Mutex;

use twizzler_abi::klog_println;

/// Master switch, **off by default**.
///
/// Arming the runtime census makes every alloc and free in this compartment pay two extra atomic
/// adds, and `start_sampler` adds a 1 Hz thread that prints a table for the whole boot. Both were
/// unconditional at `do_pager_start`, so every boot -- including every `--bench` boot -- carried
/// them. That is the exact shape of `sysbench.md`'s F11, where `perfmark` switched on syscall
/// timing before every bench and inflated absolutes by up to 2.34x. Turn this on for a leak run;
/// leave it off for a measurement.
pub const ENABLED: bool = false;

const NR_BRANCH: usize = 16;
const NR_CLASSES: usize = 32;
const NR_WORDS: usize = NR_BRANCH + NR_CLASSES * 4;

/// Same order as `runtime/alloc.rs::census`'s `B_*`. Counts in `w[i]`, bytes in `w[i + 8]`.
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

unsafe extern "C" {
    fn __twz_rt_diag_heap_census(out: *mut u64, n: usize) -> usize;
    fn __twz_rt_diag_heap_census_arm() -> u64;
    fn __twz_rt_diag_heap_objects(out: *mut u64, n: usize) -> usize;
}

/// Snapshot every Nth tick. `pager_lookup_external` runs about once per compartment load.
const PERIOD: u64 = 64;

static SEQ: Mutex<u64> = Mutex::new(0);
static PREV: Mutex<Option<[u64; NR_WORDS]>> = Mutex::new(None);

/// One allocation of a size nothing else here uses, made and freed immediately after arming.
///
/// An all-zero census reads identically whether pager-srv allocated nothing or the hooks never see
/// its allocations at all. The first snapshot must show `le=65536` with `alloc >= 1` and
/// `free >= 1`; if it does not, read nothing else in the table. (Learned the hard way: the
/// monitor's census control allocated 131072 B while its *tracker* window was [2049, 4096], so the
/// control exercised an instrument nobody was reading.)
const CONTROL_SIZE: usize = 40960;

fn positive_control() {
    // Push, then escape the pointer: `with_capacity` + `black_box(capacity)` is elided outright by
    // LLVM, which has already produced three leakcheck ops that compiled to no `__rust_alloc`.
    let mut v: Vec<u8> = Vec::with_capacity(CONTROL_SIZE);
    v.push(0xa5);
    core::hint::black_box(v.as_ptr());
    drop(v);
    klog_println!(
        "PAGER-HEAPCENSUS-CONTROL alloc_and_free_bytes={}",
        CONTROL_SIZE
    );
}

/// Every heap object this compartment's allocator actually owns.
///
/// This is a check on *attribution*, not on the leak: it is the definitive answer to "is the object
/// the grower list blames on pager-srv really pager-srv's?", independent of the `heap:<sctx>` note.
fn heap_objects_line() {
    let mut buf = [0u64; 3 * 64 + 2];
    let n = unsafe { __twz_rt_diag_heap_objects(buf.as_mut_ptr(), buf.len()) };
    if n < 2 {
        klog_println!("PAGER-HEAPOBJ unavailable");
        return;
    }
    klog_println!("PAGER-HEAPOBJ main={} early={}", buf[n - 2], buf[n - 1]);
    for i in (0..n - 2).step_by(3) {
        klog_println!(
            "PAGER-HEAPOBJ-ID slot={} id={:x}{:016x} kind={}",
            buf[i],
            buf[i + 1],
            buf[i + 2],
            if (i / 3) < buf[n - 2] as usize {
                "main"
            } else {
                "early"
            }
        );
    }
}

/// Arm the runtime's census for this compartment. Called once, from `do_pager_start`.
pub fn arm() {
    if !ENABLED {
        return;
    }
    let was = unsafe { __twz_rt_diag_heap_census_arm() };
    // Reports the prior state so "armed by us" and "already armed" stay distinguishable.
    klog_println!("PAGER-HEAPCENSUS-ARM was_already_armed={}", was);
    positive_control();
}

/// Sample on a timer rather than on a gate.
///
/// The gate-driven version armed and never fired: `l7p-loader` does not call
/// `pager_lookup_external`. Whether a service's allocations are sampled must not depend on which
/// gates the workload happens to take, so this owns its own thread. Snapshots are not aligned to
/// leakcheck's op boundaries; bucket them by line number against the `LEAKCHECK-OP` lines in the
/// same log, which is how the monitor census is already read.
pub fn start_sampler() {
    if !ENABLED {
        return;
    }
    std::thread::spawn(|| loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
        snapshot();
    });
    klog_println!("PAGER-HEAPCENSUS-SAMPLER started period_ms=1000");
}

/// Snapshot on every [`PERIOD`]th call. Cheap enough to sit on a gate.
pub fn tick() {
    if !ENABLED {
        return;
    }
    {
        let mut seq = SEQ.lock().unwrap();
        *seq += 1;
        if *seq % PERIOD != 0 {
            return;
        }
    }
    snapshot();
}

fn snapshot() {
    let mut cur = [0u64; NR_WORDS];
    let n = unsafe { __twz_rt_diag_heap_census(cur.as_mut_ptr(), NR_WORDS) };
    if n != NR_WORDS {
        // An unarmed census returns 0 and leaves the buffer zeroed, which is the same table a
        // compartment that allocated nothing would produce. Say which one this is.
        klog_println!("PAGER-HEAPCENSUS unavailable (not armed) got={}", n);
        return;
    }
    let prev = {
        let mut g = PREV.lock().unwrap();
        let p = g.unwrap_or([0u64; NR_WORDS]);
        *g = Some(cur);
        p
    };
    let d = |i: usize| cur[i] as i64 - prev[i] as i64;

    // A nonzero `drop_*` means growth is the runtime throwing frees away rather than retention --
    // a different bug with a different fix, so it must not be summed into the same number.
    let mut branches = String::new();
    for (i, name) in BRANCH.iter().enumerate() {
        if cur[i] == 0 && cur[i + 8] == 0 {
            continue;
        }
        branches.push_str(&format!(
            " {}={}/{}(+{}/{})",
            name,
            cur[i],
            cur[i + 8],
            d(i),
            d(i + 8)
        ));
    }
    klog_println!("PAGER-HEAPCENSUS total_then_delta:{}", branches);

    let mut tot: Vec<(usize, i64, i64, u64)> = Vec::new();
    let mut tot_bytes: i64 = 0;
    for c in 0..NR_CLASSES {
        let b = NR_BRANCH + c * 4;
        let (ac, ab, fc, fb) = (
            cur[b] as i64,
            cur[b + 1] as i64,
            cur[b + 2] as i64,
            cur[b + 3] as i64,
        );
        if ac == 0 && fc == 0 {
            continue;
        }
        tot_bytes += ab - fb;
        tot.push((c, ac - fc, ab - fb, cur[b]));
    }
    tot.sort_by_key(|r| -(r.2.abs()));
    klog_println!(
        "PAGER-HEAPCENSUS-TOTAL net_bytes={} classes={}",
        tot_bytes,
        tot.len()
    );
    for (c, net_count, net_bytes, allocs) in tot.into_iter() {
        klog_println!(
            "PAGER-HEAPCENSUS-TOTALCLASS le={} allocs={} net_count={} net_bytes={}",
            1u64 << c,
            allocs,
            net_count,
            net_bytes,
        );
    }
    heap_objects_line();
}
