//! leakcheck -- an experiment harness for finding real leaks.
//!
//! Runs each operation N times, samples every leak-relevant counter after each iteration, and fits
//! a line to the tail. The design centre is that most reclamation in this system is deferred,
//! cached, or both: a before/after delta around one operation measures caching, not leaking. See
//! `leakplan.md` for the deferral mechanisms and their cadences.
//!
//! Output goes through the kernel console (not stdout) so a serial log always has it, one line per
//! sample and one per fit, both machine-readable. `tools/leakplot.py` turns a log into graphs.

mod census;
mod fit;
mod ops;
mod quiesce;
mod sample;
mod uheap;

use sample::{COUNTERS, Kind, NR_COUNTERS, Sample};
use twizzler_abi::syscall::{
    KALLOC_NR_BUCKETS, KALLOC_TRACK_ARM, KALLOC_TRACK_DUMP, KALLOC_TRACK_OFF, KallocCensus,
    sys_kalloc_census, sys_kalloc_track,
};

pub fn console(s: &str) {
    twizzler_abi::syscall::sys_kernel_console_write(
        twizzler_abi::syscall::KernelConsoleSource::Console,
        s.as_bytes(),
        twizzler_abi::syscall::KernelConsoleWriteFlags::DONT_BUFFER,
    );
}

macro_rules! out {
    ($($arg:tt)*) => { console(&format!($($arg)*)) };
}

struct Config {
    iters: usize,
    warmup: usize,
    quiesce_ms: u64,
    /// Floor on each quiesce, so a TTL cache expires before the census runs. 0 = old behaviour.
    quiesce_min_ms: u64,
    ops: Vec<String>,
    samples: bool,
    census: bool,
    /// `--track lo:hi`: arm the kernel's live-block tracker over this size range for the duration
    /// of each op, and dump whatever is still live once the post-quiesce has run.
    track: Option<(u64, u64)>,
    /// How many times to run each op when tracking. **Two by default, and the second pass is the
    /// point.** A tracked window reports the blocks allocated inside it and not freed, which a
    /// one-time fill -- a cache, a lazily-populated table, a high-water reserve -- produces just as
    /// readily as a leak does. Repeating the identical op in the same boot separates them: a
    /// per-iteration leak must retain at the same rate every time, and a fill cannot. Measured on
    /// `l3-thread-x10`: 42 blocks on the first pass, 9 on the second.
    ///
    /// This is deliberately a repeat rather than a statistic computed inside one window. A
    /// heuristic over the age spread was tried first and rejected: it labels correctly on a fill
    /// that lands in a tight burst (`oldest=33 newest=233` of 6651) and mislabels the same
    /// mechanism when the fill converges slowly (`oldest=33 newest=3887` of 6642), which is the
    /// same op on a different boot. No single window separates them; two do.
    track_passes: usize,
    /// `--utrack lo:hi`: the userspace analogue of `--track`. Records every *userspace* heap block
    /// in this size range allocated during an op and not freed, and prints the first 32 bytes of
    /// each. A size class from `LEAKCHECK-UHEAP` says what is retained; this says what is in it.
    utrack: Option<(usize, usize)>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // 40 iterations leaves a 30-sample tail after warmup, which is enough for a slope to
            // separate from the noise of a counter that moves in whole frames.
            iters: 40,
            // The handle cache holds a released handle for 2s and several caches fill on first
            // use, so the early iterations are not in steady state and must not be fitted.
            warmup: 10,
            quiesce_ms: 4000,
            quiesce_min_ms: 0,
            ops: ops::DEFAULT_OPS.iter().map(|s| s.to_string()).collect(),
            samples: true,
            census: false,
            track: None,
            utrack: None,
            track_passes: 2,
        }
    }
}

fn parse_args() -> Config {
    let mut cfg = Config::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let mut next = || {
            i += 1;
            args.get(i).cloned().unwrap_or_default()
        };
        match a {
            "-n" | "--iters" => cfg.iters = next().parse().unwrap_or(cfg.iters),
            "--warmup" => cfg.warmup = next().parse().unwrap_or(cfg.warmup),
            "--quiesce-ms" => cfg.quiesce_ms = next().parse().unwrap_or(cfg.quiesce_ms),
            "--quiesce-min-ms" => {
                cfg.quiesce_min_ms = next().parse().unwrap_or(cfg.quiesce_min_ms)
            }
            "--ops" => {
                let v = next();
                cfg.ops = if v == "all" {
                    ops::OPS.iter().map(|o| o.name.to_string()).collect()
                } else {
                    v.split(',').map(|s| s.trim().to_string()).collect()
                };
            }
            "--utrack" => {
                let v = next();
                let (lo, hi) = v.split_once(':').unwrap_or(("0", "0"));
                cfg.utrack = Some((lo.parse().unwrap_or(0), hi.parse().unwrap_or(0)));
            }
            "--no-samples" => cfg.samples = false,
            "--census" => cfg.census = true,
            "--track-passes" => {
                cfg.track_passes = next().parse().unwrap_or(cfg.track_passes).max(1)
            }
            "--track" => {
                let v = next();
                let mut it = v.split(':');
                let lo = it.next().and_then(|s| s.trim().parse::<u64>().ok());
                let hi = it.next().and_then(|s| s.trim().parse::<u64>().ok());
                cfg.track = match (lo, hi) {
                    (Some(lo), Some(hi)) => Some((lo, hi)),
                    _ => None,
                };
            }
            "--help" | "-h" => {
                out!(
                    "leakcheck [-n N] [--warmup W] [--quiesce-ms MS] [--ops a,b|all] [--no-samples] [--census] [--track lo:hi] [--track-passes N]\n"
                );
                out!("ops: ");
                for o in ops::OPS {
                    out!("{} ", o.name);
                }
                out!("\n");
                std::process::exit(0);
            }
            _ => {}
        }
        i += 1;
    }
    cfg
}

fn main() {
    // Spawned by `l7-spawn-ls` as a child. Exit before anything else runs: no counters, no ops, and
    // above all no recursion. This is checked before argument parsing so a malformed argv cannot
    // reach the op loop in a child.
    if std::env::args().any(|a| a == "--child-exit") {
        std::process::exit(0);
    }
    let cfg = parse_args();
    dump_self_map("boot");
    out!(
        "LEAKCHECK-BEGIN iters={} warmup={} quiesce_ms={} quiesce_min_ms={} counters={}\n",
        cfg.iters,
        cfg.warmup,
        cfg.quiesce_ms,
        cfg.quiesce_min_ms,
        NR_COUNTERS
    );
    // The TLS template layout, because it is the unit a per-thread TLS leak would come in. A
    // measured per-spawn retention that matches this to within a page is strong evidence the leak
    // is exactly one region per thread; one that does not match rules that out arithmetically.
    let cc = monitor_api::get_comp_config();
    let tls = unsafe { cc.get_tls_template().as_ref() };
    match tls {
        Some(t) => out!(
            "LEAKCHECK-TLS layout_bytes={} layout_pages={:.2} align={} gen={}\n",
            t.layout.size(),
            t.layout.size() as f64 / 4096.0,
            t.layout.align(),
            t.r#gen
        ),
        None => out!("LEAKCHECK-TLS unavailable\n"),
    }

    pthread_dtor_probe();
    uheap::arm();
    uheap::report_compartment_sctxs();

    for (i, c) in COUNTERS.iter().enumerate() {
        out!(
            "LEAKCHECK-COUNTER {} {} {}\n",
            i,
            c.name,
            if c.kind == Kind::Level { "level" } else { "cumulative" }
        );
    }

    // One sample buffer for the whole run, fully touched before any op's baseline is taken. The
    // buffer is the instrument's own footprint — 248 B per sample, 0.0605 pages/iter — and with
    // lazy touching it was the entire post-fix null floor (leak26: near-miss 0.060, arithmetic
    // exact). Paying every page here, outside all measurement windows, makes the instrument
    // invisible to itself; leakplan §11 confounder #1, closed.
    let mut series: Vec<Sample> = Vec::new();
    series.resize(cfg.iters, Sample { v: [0; sample::NR_COUNTERS] });
    std::hint::black_box(&series);
    series.clear();

    for name in &cfg.ops {
        let Some(op) = ops::OPS.iter().find(|o| o.name == *name) else {
            out!("LEAKCHECK-SKIP {} unknown-op\n", name);
            continue;
        };
        let passes = if cfg.track.is_some() { cfg.track_passes } else { 1 };
        for pass in 1..=passes {
            if passes > 1 {
                out!("LEAKCHECK-PASS {} {}/{}\n", op.name, pass, passes);
            }
            run_op(op, &cfg, &mut series, pass);
        }
    }

    out!("LEAKCHECK-END\n");
}

/// Every object mapped into this compartment, by slot.
///
/// `LEAKCHECK-CENSUS` names growers by object id and can report `note=heap`, but "a heap" is not
/// "whose heap" -- and with `mon.compartments`, `mon.threads` and `self.slots` all flat under
/// `l7-spawn-proc` while two long-lived `heap` objects gain 34 pages/iter between them, whose heap
/// it is *is* the finding. A grower in this list is ours; a grower absent from it belongs to
/// another compartment.
fn dump_self_map(tag: &str) {
    use twizzler_abi::syscall::sys_object_read_map;
    let mut n = 0;
    for slot in 0..twizzler_abi::arch::SLOTS {
        if let Ok(info) = sys_object_read_map(None, slot) {
            out!(
                "LEAKCHECK-SELFMAP {} slot={} id={:x} prot={:?}\n",
                tag,
                slot,
                info.id.raw(),
                info.prot
            );
            n += 1;
            if n > 512 {
                break;
            }
        }
    }
    out!("LEAKCHECK-SELFMAP-END {} count={}\n", tag, n);
}

fn run_op(op: &ops::Op, cfg: &Config, series: &mut Vec<Sample>, pass: usize) {
    let mut state = (op.setup)();
    series.clear();

    // Quiesce before the baseline as well as after: whatever the previous operation deferred would
    // otherwise be reclaimed during this one's tail and show up as a negative slope.
    let pre = quiesce::quiesce(cfg.quiesce_ms, cfg.quiesce_min_ms);
    let base = Sample::take();
    // After the quiesce, so anything the previous op deferred is already reclaimed and does not
    // read as this op's growth.
    let census_before = cfg.census.then(census::take);
    let kalloc_before = sys_kalloc_census();
    // Unconditional: the counters are always collected, and a per-op readout costs one line. The
    // op that needs it most is whichever one turns out to leak, which is not known in advance.
    let uheap_before = uheap::take();
    if let Some((lo, hi)) = cfg.utrack {
        uheap::track_arm(lo, hi);
    }
    // Armed after the pre-quiesce so the table holds this op's allocations and nothing else: the
    // residual is scale-triggered, and a table that also held the boot's live set would overflow
    // long before the op ran.
    if let Some((lo, hi)) = cfg.track {
        sys_kalloc_track(KALLOC_TRACK_ARM, lo, hi);
    }

    for _ in 0..cfg.iters {
        (op.run)(&mut state);
        series.push(Sample::take());
    }

    let post = quiesce::quiesce(cfg.quiesce_ms, cfg.quiesce_min_ms);
    let settled = Sample::take();
    // **First, before the other two.** `census::take` builds a HashMap over every object in the
    // system; taken before this snapshot it lands *inside* the heap-census window and is still
    // live when the window closes, reading as one retained 16,912-byte block in every op --
    // bit-identical across four runs, which was the tell. The before-side already has this
    // ordering, which is why only the after-side leaked in.
    let uheap_after = uheap::take();
    let census_after = cfg.census.then(census::take);
    let kalloc_after = sys_kalloc_census();
    // After the post-quiesce, so every deferred free has run: what is left is retained, not in
    // flight. The dump itself prints from the kernel (KALLOC-TRACK-*), out of the alloc path.
    if cfg.track.is_some() {
        let t = sys_kalloc_track(KALLOC_TRACK_DUMP, 0, 0);
        // `pass` is on this line and not on the others so that every existing line keeps its
        // format and `leakplot.py` keeps parsing; the pass number only ever matters here.
        out!(
            "LEAKCHECK-TRACK {}#{} live={} inserted={} removed={} overflow={} free_miss={}\n",
            op.name, pass, t.live, t.inserted, t.removed, t.overflow, t.free_miss
        );
        sys_kalloc_track(KALLOC_TRACK_OFF, 0, 0);
    }

    let failed = ops::failures(&state);
    if failed > 0 {
        out!(
            "LEAKCHECK-SKIP {} {}/{}-iterations-failed\n",
            op.name,
            failed,
            cfg.iters
        );
        return;
    }

    uheap::report(op.name, &uheap_before, &uheap_after, cfg.iters);
    // Per op, not once: the question is whether the object that *grew during this op* belongs to
    // this compartment's allocator, and the answer can change mid-run -- talc claims a new heap
    // object when the current one fills, so an op's grower may be an object that did not exist when
    // the op started.
    uheap::report_own_heaps(op.name);
    ops::sctxlive_report(&state);
    if cfg.utrack.is_some() {
        uheap::track_report(op.name, cfg.iters);
        uheap::track_off();
    }

    // Both ends of the decommit path, per op: how often ferroc asked us to give frames back, and
    // how often we declined because the range's object could not be named. A zero in the first
    // column and a zero in the second mean opposite things.
    {
        let mut d = [0u64; 8];
        unsafe { __twz_rt_diag_decommit_stats(d.as_mut_ptr()) };
        out!(
            "LEAKCHECK-DECOMMIT {} hook_decommit={} hook_dealloc={} ranges={} no_id={} bytes_declined={} base_alloc={}/{} base_dealloc_bytes={}\n",
            op.name, d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]
        );
    }

    out!(
        "LEAKCHECK-OP {} iters={} pre_converged={} pre_ms={} post_converged={} post_ms={}\n",
        op.name,
        cfg.iters,
        pre.converged,
        pre.elapsed_ms,
        post.converged,
        post.elapsed_ms
    );

    if cfg.samples {
        for (i, s) in series.iter().enumerate() {
            let mut line = format!("LEAKCHECK-SAMPLE {} {}", op.name, i);
            for v in &s.v {
                line.push(' ');
                line.push_str(&v.to_string());
            }
            line.push('\n');
            console(&line);
        }
    }

    // The tail, and only the tail. Warmup iterations are steady-state cost, not leak.
    let start = cfg.warmup.min(series.len().saturating_sub(3));
    for ci in 0..NR_COUNTERS {
        let ys: Vec<u64> = series[start..].iter().map(|s| s.v[ci]).collect();
        let Some(f) = fit::fit(&ys) else {
            out!("LEAKCHECK-FIT {} {} absent\n", op.name, COUNTERS[ci].name);
            continue;
        };
        // Net change across the whole operation, measured between two quiesced states: this is the
        // number that says whether anything was actually retained, as opposed to merely in flight.
        let net = settled.v[ci] as i64 - base.v[ci] as i64;
        out!(
            "LEAKCHECK-FIT {} {} {} slope={:.4} r2={:.4} growth={:.1} duty={:.3} maxstep={:.3} net={} n={}\n",
            op.name,
            COUNTERS[ci].name,
            if COUNTERS[ci].kind == Kind::Level { "level" } else { "cumulative" },
            f.slope,
            f.r2,
            f.growth,
            f.duty,
            f.max_step_frac,
            net,
            f.n
        );
    }

    report_kalloc(op.name, &kalloc_before, &kalloc_after, cfg.iters);

    if let (Some(b), Some(a)) = (census_before.as_ref(), census_after.as_ref()) {
        report_census(op.name, b, a, cfg.iters);
    }

    dump_self_map(op.name);

    verdict(op.name, &series[start..], &base, &settled);
}

/// Objects that gained pages across the operation, biggest first. Only the top few matter: the
/// point is to name the holder, and a long tail of one-page movers is background.
fn report_census(op: &str, before: &census::Census, after: &census::Census, iters: usize) {
    let deltas = census::diff(before, after);
    let total: i64 = deltas.iter().map(|d| d.growth()).sum();
    out!(
        "LEAKCHECK-CENSUS {} objects_before={} objects_after={} growers={} pages_gained={} unstattable={}/{}\n",
        op,
        before.pages.len(),
        after.pages.len(),
        deltas.len(),
        total,
        before.unstattable,
        after.unstattable
    );
    for d in deltas.iter().take(40) {
        let per_iter = d.growth() as f64 / iters as f64;
        let n = census::note(d.id).unwrap_or_else(|| "-".to_string());
        out!(
            "LEAKCHECK-GROWER {} {:x} pages {}->{} (+{}, {:.2}/iter) {} note={}\n",
            op,
            d.id.raw(),
            d.before,
            d.after,
            d.growth(),
            per_iter,
            if d.is_new { "new" } else { "existing" },
            n
        );
    }
    report_grower_histograms(op, &deltas, iters);
}

/// The list above is truncation-blind: `leak1` printed 40 of 448 growers, so the population that
/// carried the count was never visible. These cover every grower, so a silence here is a
/// measurement rather than a cap. `COVER` is the arithmetic check -- `covered` must equal
/// `growers`, or something is filtering between the two.
fn report_grower_histograms(op: &str, deltas: &[census::Delta], iters: usize) {
    use std::collections::HashMap;

    let mut by_note: HashMap<String, (usize, usize, i64)> = HashMap::new();
    let mut by_size: HashMap<i64, (usize, usize)> = HashMap::new();
    for d in deltas.iter() {
        let n = census::note(d.id).unwrap_or_else(|| "-".to_string());
        let e = by_note.entry(n).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += d.is_new as usize;
        e.2 += d.growth();
        let s = by_size.entry(d.growth()).or_insert((0, 0));
        s.0 += 1;
        s.1 += d.is_new as usize;
    }

    let mut notes: Vec<_> = by_note.into_iter().collect();
    notes.sort_by_key(|(_, v)| -(v.0 as i64));
    let covered: usize = notes.iter().map(|(_, v)| v.0).sum();
    for (note, (count, new, pages)) in notes.iter() {
        out!(
            "LEAKCHECK-GROWER-BYNOTE {} note={} count={} new={} pages={} per_iter={:.4}\n",
            op,
            note,
            count,
            new,
            pages,
            *count as f64 / iters as f64
        );
    }

    let mut sizes: Vec<_> = by_size.into_iter().collect();
    sizes.sort_by_key(|(_, v)| -(v.0 as i64));
    for (pages, (count, new)) in sizes.iter().take(24) {
        out!(
            "LEAKCHECK-GROWER-BYSIZE {} pages={} count={} new={} per_iter={:.4}\n",
            op,
            pages,
            count,
            new,
            *count as f64 / iters as f64
        );
    }

    out!(
        "LEAKCHECK-GROWER-COVER {} growers={} covered={} distinct_notes={} distinct_sizes={}\n",
        op,
        deltas.len(),
        covered,
        notes.len(),
        sizes.len()
    );
}

/// No single step may account for more than this much of the tail's growth. Above it, the counter
/// moved in jumps -- background work -- rather than accruing per iteration. Set from the observed
/// null control, which climbed 8 pages in two jumps of 4 (0.5) against the positive control's
/// even climb (~0.05). See `Fit::max_step_frac`.
const MAX_STEP_FRAC: f64 = 0.34;

/// The in-guest summary. Deliberately conservative: a counter is flagged only when the slope is
/// well-explained, the growth is spread across the tail rather than concentrated in a jump, *and*
/// the net change between two quiesced states agrees. Any one of those alone is how a deferred
/// reclaim or a bit of background work gets misreported as a leak.
fn verdict(op: &str, tail: &[Sample], base: &Sample, settled: &Sample) {
    let mut flagged = 0;
    for ci in 0..NR_COUNTERS {
        if COUNTERS[ci].kind != Kind::Level {
            continue;
        }
        let ys: Vec<u64> = tail.iter().map(|s| s.v[ci]).collect();
        let Some(f) = fit::fit(&ys) else { continue };
        let net = settled.v[ci] as i64 - base.v[ci] as i64;
        if f.slope > 0.0
            && f.r2 >= 0.9
            && f.growth >= 1.0
            && f.max_step_frac <= MAX_STEP_FRAC
            && net > 0
        {
            out!(
                "LEAKCHECK-LEAK {} {} slope={:.4}/iter net={} r2={:.3} duty={:.2} maxstep={:.2}\n",
                op,
                COUNTERS[ci].name,
                f.slope,
                net,
                f.r2,
                f.duty,
                f.max_step_frac
            );
            flagged += 1;
        }
    }
    if flagged == 0 {
        out!("LEAKCHECK-CLEAN {}\n", op);
    }
}

unsafe extern "C" {
    /// Decommit-path counters in the reference runtime (`runtime/alloc/ferroc.rs`).
    fn __twz_rt_diag_decommit_stats(out: *mut u64);
    fn pthread_key_create(
        key: *mut usize,
        dtor: Option<unsafe extern "C" fn(*mut core::ffi::c_void)>,
    ) -> i32;
    fn pthread_setspecific(key: usize, value: *const core::ffi::c_void) -> i32;
}

static DTOR_RUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

unsafe extern "C" fn probe_dtor(_: *mut core::ffi::c_void) {
    DTOR_RUNS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
}

/// Do mlibc's pthread-key destructors run when a spawned thread exits?
///
/// A regression guard, now that they do. ferroc releases a thread's heap by registering
/// `ThreadLocal::put(id)` through `pthread_key_create`, and recycling that id is what lets the next
/// spawn reuse the dead thread's context and slabs. Until `InternalThread::drop` called
/// `__mlibc_handle_thread_exit`, nothing on the spawn path reached mlibc's destructor machinery at
/// all, and every spawn took a fresh 4 MiB slab it never gave back. `ran` dropping below `set` means
/// that call has been lost again.
fn pthread_dtor_probe() {
    use std::sync::atomic::Ordering::SeqCst;
    const N: usize = 10;
    let mut key: usize = 0;
    let rc = unsafe { pthread_key_create(&mut key, Some(probe_dtor)) };
    if rc != 0 {
        out!("LEAKCHECK-PTHREAD-DTOR key_create_failed={}\n", rc);
        return;
    }
    let mut set_ok = 0usize;
    for _ in 0..N {
        let h = std::thread::spawn(move || unsafe {
            pthread_setspecific(key, core::ptr::without_provenance(1))
        });
        if h.join().map(|r| r == 0).unwrap_or(false) {
            set_ok += 1;
        }
    }
    // `impl_join` reaps the entry synchronously, so a returned join has had its chance. The settle
    // only covers a reap that lands on a later gc sweep.
    std::thread::sleep(std::time::Duration::from_millis(200));
    out!(
        "LEAKCHECK-PTHREAD-DTOR key={} set={}/{} ran={}\n",
        key,
        set_ok,
        N,
        DTOR_RUNS.load(SeqCst)
    );
}

/// Kernel-heap allocation deltas by size class, per operation.
///
/// `mem.kalloc_bytes` is net-live kernel heap, so a slope on it is bytes the kernel never freed;
/// this says which size class holds them. Gross alloc/free counts are printed alongside the net
/// because they answer different questions: a class with 8,800 allocations and 8,580 frees is a
/// churny path retaining a few, and a class with 220 allocations and 0 frees is a per-iteration
/// leak. Only classes that moved are printed.
fn report_kalloc(op: &str, before: &KallocCensus, after: &KallocCensus, iters: usize) {
    let mut rows: Vec<(usize, u64, u64, i64, i64)> = Vec::new();
    let mut total_net_bytes = 0i64;
    for b in 0..KALLOC_NR_BUCKETS {
        let (x, y) = (&before.buckets[b], &after.buckets[b]);
        let ac = y.alloc_count.saturating_sub(x.alloc_count);
        let fc = y.free_count.saturating_sub(x.free_count);
        let ab = y.alloc_bytes.saturating_sub(x.alloc_bytes) as i64;
        let fb = y.free_bytes.saturating_sub(x.free_bytes) as i64;
        if ac == 0 && fc == 0 {
            continue;
        }
        let nb = ab - fb;
        total_net_bytes += nb;
        rows.push((b, ac, fc, ac as i64 - fc as i64, nb));
    }
    out!(
        "LEAKCHECK-KALLOC-TOTAL {} net_bytes={} per_iter={:.1} classes={} unbalanced={}\n",
        op,
        total_net_bytes,
        total_net_bytes as f64 / iters as f64,
        rows.len(),
        rows.iter().filter(|r| r.3 != 0).count()
    );
    // Every class whose count did not balance, not the top N by bytes. The top-N form lost the
    // finding it was built for: a boot whose trap symbolized in-window retained 878 KB of DWARF
    // context, which occupied the whole table and truncated the 800-byte class being hunted. A
    // class that allocated and freed in equal numbers is the uninteresting case and stays cut.
    rows.sort_by_key(|r| -(r.4.abs()));
    let unbalanced = rows.iter().filter(|r| r.3 != 0).count();
    for (b, ac, fc, nc, nb) in rows
        .into_iter()
        .filter(|r| r.3 != 0)
        .take(40)
        .collect::<Vec<_>>()
    {
        let _ = unbalanced;
        out!(
            "LEAKCHECK-KALLOC {} size={} alloc={} free={} net_count={} net_bytes={} per_iter={:.2}\n",
            op,
            KallocCensus::bucket_size(b),
            ac,
            fc,
            nc,
            nb,
            nb as f64 / iters as f64
        );
    }
}
