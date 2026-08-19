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

use sample::{COUNTERS, Kind, NR_COUNTERS, Sample};

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
    ops: Vec<String>,
    samples: bool,
    census: bool,
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
            ops: ops::DEFAULT_OPS.iter().map(|s| s.to_string()).collect(),
            samples: true,
            census: false,
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
            "--ops" => {
                let v = next();
                cfg.ops = if v == "all" {
                    ops::OPS.iter().map(|o| o.name.to_string()).collect()
                } else {
                    v.split(',').map(|s| s.trim().to_string()).collect()
                };
            }
            "--no-samples" => cfg.samples = false,
            "--census" => cfg.census = true,
            "--help" | "-h" => {
                out!(
                    "leakcheck [-n N] [--warmup W] [--quiesce-ms MS] [--ops a,b|all] [--no-samples] [--census]\n"
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
    out!(
        "LEAKCHECK-BEGIN iters={} warmup={} quiesce_ms={} counters={}\n",
        cfg.iters,
        cfg.warmup,
        cfg.quiesce_ms,
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

    for (i, c) in COUNTERS.iter().enumerate() {
        out!(
            "LEAKCHECK-COUNTER {} {} {}\n",
            i,
            c.name,
            if c.kind == Kind::Level { "level" } else { "cumulative" }
        );
    }

    for name in &cfg.ops {
        let Some(op) = ops::OPS.iter().find(|o| o.name == *name) else {
            out!("LEAKCHECK-SKIP {} unknown-op\n", name);
            continue;
        };
        run_op(op, &cfg);
    }

    out!("LEAKCHECK-END\n");
}

fn run_op(op: &ops::Op, cfg: &Config) {
    let mut state = (op.setup)();

    // Quiesce before the baseline as well as after: whatever the previous operation deferred would
    // otherwise be reclaimed during this one's tail and show up as a negative slope.
    let pre = quiesce::quiesce(cfg.quiesce_ms);
    let base = Sample::take();
    // After the quiesce, so anything the previous op deferred is already reclaimed and does not
    // read as this op's growth.
    let census_before = cfg.census.then(census::take);

    let mut series: Vec<Sample> = Vec::with_capacity(cfg.iters);
    for _ in 0..cfg.iters {
        (op.run)(&mut state);
        series.push(Sample::take());
    }

    let post = quiesce::quiesce(cfg.quiesce_ms);
    let settled = Sample::take();
    let census_after = cfg.census.then(census::take);

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

    // Both ends of the decommit path, per op: how often ferroc asked us to give frames back, and
    // how often we declined because the range's object could not be named. A zero in the first
    // column and a zero in the second mean opposite things.
    {
        let mut d = [0u64; 5];
        unsafe { __twz_rt_diag_decommit_stats(d.as_mut_ptr()) };
        out!(
            "LEAKCHECK-DECOMMIT {} hook_decommit={} hook_dealloc={} ranges={} no_id={} bytes_declined={}\n",
            op.name, d[0], d[1], d[2], d[3], d[4]
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

    if let (Some(b), Some(a)) = (census_before.as_ref(), census_after.as_ref()) {
        report_census(op.name, b, a, cfg.iters);
    }

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
    for d in deltas.iter().take(12) {
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
