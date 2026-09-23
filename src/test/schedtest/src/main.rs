//! Scheduler measurements: throughput and fairness of cpu-bound threads, yield and hand-off
//! rates, wake lateness of a sleeper among spinners, memory bandwidth by working-set size and
//! access pattern, and spawn rate -- each at thread counts from 1 to 3N for N cpus.
//!
//! Every result is one `st <test> threads=T k=v ...` line. `switches`/`migrations` are the
//! kernel's per-thread counters over the run, `cpus` where each thread ended up, `k_*` the
//! kernel-wide deltas; `preempts` is what the threads saw themselves (a gap over `GAP` between
//! consecutive clock reads in the hot loop), which also catches host steal, so `steal_ms` from
//! `sys_info` is printed alongside.
//!
//! `st [--secs S] [--threads a,b,c] [test ...]`; tests: spin yield pingpong sleeper memory spawn.
//! Each test runs `S` seconds (default 5) per thread count, long enough for the 0.5-1.5 s
//! rebalance to act; memory runs each size/pattern for `S/4`. `spin` also reports fairness over
//! its first and last second, so placement and steady state can be told apart.
//!
//! Builds for Linux and FreeBSD too (`rustc -O main.rs`), where the same counters come from
//! `getrusage`, `sched_getcpu`, procfs and sysctl -- see `os`. Migrations are 0 where the OS does
//! not count them.

use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

use os::{cpu_summary, kernel_delta, steal_ms, thread_counters, KernelSnapshot};

const GAP: Duration = Duration::from_micros(100);
/// Hot-loop work between clock reads; a few microseconds, so a `GAP` is time off the cpu.
const BATCH: u64 = 4096;

struct Opts {
    secs: f64,
    threads: Vec<usize>,
    tests: Vec<String>,
}

fn parse_args() -> Opts {
    let n = std::thread::available_parallelism().unwrap().get();
    let mut opts = Opts {
        secs: 5.0,
        threads: Vec::new(),
        tests: Vec::new(),
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--secs" => opts.secs = args.next().unwrap().parse().unwrap(),
            "--threads" => {
                opts.threads = args
                    .next()
                    .unwrap()
                    .split(',')
                    .map(|s| s.parse().unwrap())
                    .collect()
            }
            t => opts.tests.push(t.to_string()),
        }
    }
    if opts.threads.is_empty() {
        let mut t = vec![1, 2, n / 2, n, 3 * n / 2, 2 * n, 3 * n];
        t.retain(|&t| t >= 1);
        t.sort();
        t.dedup();
        opts.threads = t;
    }
    if opts.tests.is_empty() {
        opts.tests = ["spin", "yield", "pingpong", "sleeper", "memory", "spawn"]
            .iter()
            .map(|s| s.to_string())
            .collect();
    }
    opts
}

/// Xorshift64: the only randomness the tests need (a Sattolo shuffle and random indices).
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E3779B97F4A7C15) | 1)
    }

    #[inline]
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// Per-thread instrumentation: kernel counters at start/end, and self-observed preemption gaps.
struct Probe {
    start: (u64, u64, u32),
    last: Instant,
    preempts: u64,
    off_cpu: Duration,
}

impl Probe {
    fn start() -> Self {
        Self {
            start: thread_counters(),
            last: Instant::now(),
            preempts: 0,
            off_cpu: Duration::ZERO,
        }
    }

    /// Called from the hot loop once per `BATCH` of work.
    #[inline]
    fn tick(&mut self) {
        let now = Instant::now();
        let gap = now - self.last;
        if gap > GAP {
            self.preempts += 1;
            self.off_cpu += gap;
        }
        self.last = now;
    }

    fn finish(self) -> ProbeResult {
        let end = thread_counters();
        ProbeResult {
            switches: end.0 - self.start.0,
            migrations: end.1 - self.start.1,
            cpu: end.2,
            preempts: self.preempts,
            off_cpu: self.off_cpu,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct ProbeResult {
    switches: u64,
    migrations: u64,
    cpu: u32,
    preempts: u64,
    off_cpu: Duration,
}

impl ProbeResult {
    /// Totals, plus each thread's final cpu and switch count.
    fn fmt(rs: &[ProbeResult]) -> String {
        let a = rs.iter().fold(ProbeResult::default(), |a, r| ProbeResult {
            switches: a.switches + r.switches,
            migrations: a.migrations + r.migrations,
            cpu: 0,
            preempts: a.preempts + r.preempts,
            off_cpu: a.off_cpu + r.off_cpu,
        });
        let cpus: Vec<String> = rs.iter().map(|r| format!("{}:{}", r.cpu, r.switches)).collect();
        format!(
            "switches={} migrations={} preempts={} off_cpu_ms={:.1} cpu:switches=[{}]",
            a.switches,
            a.migrations,
            a.preempts,
            a.off_cpu.as_secs_f64() * 1e3,
            cpus.join(",")
        )
    }
}

/// Jain's fairness index over per-thread totals: 1.0 is perfectly even, 1/n is one thread
/// taking everything.
fn jain(xs: &[f64]) -> f64 {
    let s: f64 = xs.iter().sum();
    let sq: f64 = xs.iter().map(|x| x * x).sum();
    if sq == 0.0 { 1.0 } else { s * s / (xs.len() as f64 * sq) }
}

fn spread(xs: &[f64]) -> String {
    let min = xs.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = xs.iter().cloned().fold(0.0, f64::max);
    format!("min={:.0} max={:.0} jain={:.3}", min, max, jain(xs))
}

/// Run `f` on `threads` threads released together, for the caller's chosen duration.
fn run<R: Send + 'static>(
    threads: usize,
    f: impl Fn(usize, Arc<Barrier>) -> R + Send + Sync + 'static,
) -> Vec<R> {
    let f = Arc::new(f);
    let barrier = Arc::new(Barrier::new(threads));
    let hs: Vec<_> = (0..threads)
        .map(|i| {
            let f = f.clone();
            let b = barrier.clone();
            std::thread::spawn(move || f(i, b))
        })
        .collect();
    hs.into_iter().map(|h| h.join().unwrap()).collect()
}

fn spin(threads: usize, secs: f64) {
    let steal0 = steal_ms();
    let k0 = KernelSnapshot::take();
    let dur = Duration::from_secs_f64(secs);
    let res = run(threads, move |_, b| {
        b.wait();
        let mut probe = Probe::start();
        let start = Instant::now();
        let mut iters = 0u64;
        let mut x = 0u64;
        // Cumulative iterations at each whole second, for the first/last-second fairness.
        let mut marks = Vec::new();
        while start.elapsed() < dur {
            for _ in 0..BATCH {
                x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
            }
            iters += BATCH;
            probe.tick();
            if start.elapsed().as_secs() as usize > marks.len() {
                marks.push(iters);
            }
        }
        (iters, marks, probe.finish())
    });
    let iters: Vec<f64> = res.iter().map(|r| r.0 as f64).collect();
    let total: f64 = iters.iter().sum();
    let first: Vec<f64> = res.iter().map(|r| r.1.first().copied().unwrap_or(r.0) as f64).collect();
    let last: Vec<f64> = res
        .iter()
        .map(|r| (r.0 - r.1.iter().rev().nth(1).copied().unwrap_or(0)) as f64)
        .collect();
    println!(
        "st spin threads={} iters_per_s={:.0} {} jain_1s={:.3} jain_last={:.3} {} {}",
        threads,
        total / secs,
        spread(&iters),
        jain(&first),
        jain(&last),
        ProbeResult::fmt(&res.iter().map(|r| r.2).collect::<Vec<_>>()),
        kernel_delta(&k0, steal0)
    );
}

fn yield_test(threads: usize, secs: f64) {
    let steal0 = steal_ms();
    let k0 = KernelSnapshot::take();
    let dur = Duration::from_secs_f64(secs);
    let res = run(threads, move |_, b| {
        b.wait();
        let mut probe = Probe::start();
        let start = Instant::now();
        let mut n = 0u64;
        while start.elapsed() < dur {
            for _ in 0..64 {
                std::thread::yield_now();
            }
            n += 64;
            probe.tick();
        }
        (n, probe.finish())
    });
    let ys: Vec<f64> = res.iter().map(|r| r.0 as f64).collect();
    let total: f64 = ys.iter().sum();
    println!(
        "st yield threads={} yields_per_s={:.0} {} {} {}",
        threads,
        total / secs,
        spread(&ys),
        ProbeResult::fmt(&res.iter().map(|r| r.1).collect::<Vec<_>>()),
        kernel_delta(&k0, steal0)
    );
}

/// Pairs hand a token back and forth with park/unpark; one round trip is two wakes.
fn pingpong(threads: usize, secs: f64) {
    let pairs = (threads / 2).max(1);
    let steal0 = steal_ms();
    let k0 = KernelSnapshot::take();
    let dur = Duration::from_secs_f64(secs);
    let turns: Arc<Vec<[AtomicBool; 2]>> = Arc::new(
        (0..pairs)
            .map(|_| [AtomicBool::new(true), AtomicBool::new(false)])
            .collect(),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let peers: Arc<Vec<[std::sync::Mutex<Option<std::thread::Thread>>; 2]>> = Arc::new(
        (0..pairs)
            .map(|_| [std::sync::Mutex::new(None), std::sync::Mutex::new(None)])
            .collect(),
    );
    let (turns2, stop2, peers2) = (turns.clone(), stop.clone(), peers.clone());
    let res = run(pairs * 2, move |i, b| {
        let (p, side) = (i / 2, i % 2);
        *peers2[p][side].lock().unwrap() = Some(std::thread::current());
        b.wait();
        let peer = loop {
            if let Some(t) = peers2[p][1 - side].lock().unwrap().clone() {
                break t;
            }
        };
        let mut probe = Probe::start();
        let start = Instant::now();
        let mut trips = 0u64;
        loop {
            while !turns2[p][side].swap(false, Ordering::Acquire) {
                if stop2.load(Ordering::Relaxed) {
                    return (trips, probe.finish());
                }
                std::thread::park();
            }
            if side == 0 && start.elapsed() >= dur {
                stop2.store(true, Ordering::Relaxed);
                peer.unpark();
                return (trips, probe.finish());
            }
            trips += 1;
            probe.tick();
            turns2[p][1 - side].store(true, Ordering::Release);
            peer.unpark();
        }
    });
    let _ = (turns, stop, peers);
    let trips: Vec<f64> = res.iter().step_by(2).map(|r| r.0 as f64).collect();
    let total: f64 = trips.iter().sum();
    println!(
        "st pingpong threads={} pairs={} trips_per_s={:.0} hop_us={:.2} {} {} {}",
        threads,
        pairs,
        total / secs,
        secs * 1e6 / (2.0 * total / pairs as f64),
        spread(&trips),
        ProbeResult::fmt(&res.iter().map(|r| r.1).collect::<Vec<_>>()),
        kernel_delta(&k0, steal0)
    );
}

/// One thread sleeping 1 ms at a time beside `threads - 1` spinners: how late does it wake?
fn sleeper(threads: usize, secs: f64) {
    let steal0 = steal_ms();
    let k0 = KernelSnapshot::take();
    let dur = Duration::from_secs_f64(secs);
    let period = Duration::from_millis(1);
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let res = run(threads, move |i, b| {
        b.wait();
        if i != 0 {
            let mut x = 0u64;
            while !stop2.load(Ordering::Relaxed) {
                x = std::hint::black_box(x.wrapping_add(1));
            }
            return Vec::new();
        }
        let mut late = Vec::new();
        let start = Instant::now();
        while start.elapsed() < dur {
            let t = Instant::now();
            std::thread::sleep(period);
            late.push(t.elapsed().saturating_sub(period).as_micros() as u64);
        }
        stop2.store(true, Ordering::Relaxed);
        late
    });
    let mut late = res[0].clone();
    late.sort();
    let n = late.len().max(1);
    let mean = late.iter().sum::<u64>() as f64 / n as f64;
    println!(
        "st sleeper threads={} spinners={} samples={} late_us mean={:.0} p50={} p99={} max={} {}",
        threads,
        threads - 1,
        late.len(),
        mean,
        late.get(n / 2).copied().unwrap_or(0),
        late.get(n * 99 / 100).copied().unwrap_or(0),
        late.last().copied().unwrap_or(0),
        kernel_delta(&k0, steal0)
    );
    let _ = stop;
}

const LINE: usize = 8; // u64s per cache line
const SIZES: &[usize] = &[16 << 10, 256 << 10, 4 << 20, 64 << 20];
const PATTERNS: &[&str] = &["seq_read", "seq_write", "rand_read", "chase"];

fn memory(threads: usize, secs: f64) {
    for &size in SIZES {
        for &pat in PATTERNS {
            let steal0 = steal_ms();
            let k0 = KernelSnapshot::take();
            let dur = Duration::from_secs_f64(secs);
            let res = run(threads, move |i, b| {
                let n = size / 8;
                let lines = n / LINE;
                let mut buf: Vec<u64> = (0..n as u64).collect();
                let mut rng = Rng::new(i as u64 + 1);
                if pat == "chase" {
                    // Sattolo: one cycle through every line, in a random order.
                    let mut order: Vec<usize> = (0..lines).collect();
                    for k in (1..lines).rev() {
                        order.swap(k, (rng.next() % k as u64) as usize);
                    }
                    for k in 0..lines {
                        buf[order[k] * LINE] = (order[(k + 1) % lines] * LINE) as u64;
                    }
                }
                b.wait();
                let mut probe = Probe::start();
                let start = Instant::now();
                let mut bytes = 0u64;
                let mut acc = 0u64;
                let mut idx = 0usize;
                while start.elapsed() < dur {
                    match pat {
                        "seq_read" => {
                            for c in buf.chunks(8192) {
                                for v in c {
                                    acc = acc.wrapping_add(*v);
                                }
                                probe.tick();
                            }
                            bytes += size as u64;
                        }
                        "seq_write" => {
                            for c in buf.chunks_mut(8192) {
                                for v in c.iter_mut() {
                                    *v = acc;
                                }
                                acc = acc.wrapping_add(1);
                                probe.tick();
                            }
                            bytes += size as u64;
                        }
                        "rand_read" => {
                            for _ in 0..BATCH {
                                let r = rng.next();
                                acc = acc.wrapping_add(buf[(r as usize % lines) * LINE]);
                            }
                            bytes += BATCH * 64;
                            probe.tick();
                        }
                        _ => {
                            for _ in 0..BATCH {
                                idx = buf[idx] as usize;
                            }
                            acc = acc.wrapping_add(idx as u64);
                            bytes += BATCH * 64;
                            probe.tick();
                        }
                    }
                }
                std::hint::black_box(acc);
                (bytes, probe.finish())
            });
            let bs: Vec<f64> = res.iter().map(|r| r.0 as f64).collect();
            let total: f64 = bs.iter().sum();
            let per = bs.iter().map(|b| b / secs / 1e9).collect::<Vec<_>>();
            println!(
                "st memory threads={} size_kb={} pattern={} gb_per_s={:.2} per_thread_gb_per_s min={:.2} max={:.2} jain={:.3} {} {}",
                threads,
                size >> 10,
                pat,
                total / secs / 1e9,
                per.iter().cloned().fold(f64::INFINITY, f64::min),
                per.iter().cloned().fold(0.0, f64::max),
                jain(&bs),
                ProbeResult::fmt(&res.iter().map(|r| r.1).collect::<Vec<_>>()),
                kernel_delta(&k0, steal0)
            );
        }
    }
}

/// Spawn `threads` threads that exit at once and join them, repeatedly.
fn spawn(threads: usize, secs: f64) {
    let steal0 = steal_ms();
    let k0 = KernelSnapshot::take();
    let dur = Duration::from_secs_f64(secs);
    let start = Instant::now();
    let mut rounds = 0u64;
    let counter = Arc::new(AtomicU64::new(0));
    while start.elapsed() < dur {
        let hs: Vec<_> = (0..threads)
            .map(|_| {
                let c = counter.clone();
                std::thread::spawn(move || {
                    c.fetch_add(1, Ordering::Relaxed);
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        rounds += 1;
    }
    let el = start.elapsed().as_secs_f64();
    println!(
        "st spawn threads={} spawns_per_s={:.0} round_us={:.0} {}",
        threads,
        counter.load(Ordering::Relaxed) as f64 / el,
        el * 1e6 / rounds as f64,
        kernel_delta(&k0, steal0)
    );
}

#[cfg(target_os = "twizzler")]
mod cache_test {
    //! The cache-miss penalty test (twizzler-bd / twizzler-48): needs thread affinity and the
    //! per-thread penalty/miss counters, so Twizzler only.
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::{Duration, Instant},
    };

    use twizzler_abi::syscall::{sys_thread_self_id, sys_thread_set_affinity, CpuMask};

    use super::{
        os::{cpu_infos, kernel_delta, steal_ms, thread_stats, KernelSnapshot},
        run, Probe, ProbeResult, BATCH,
    };

    const HOSTILE_BYTES: usize = 64 << 20;
    const FRIENDLY_BYTES: usize = 32 << 10;

    /// Random read-modify-writes over a buffer far bigger than any cache: a miss per access.
    /// `seed` differs per thread, or every hostile walks the same lines in lockstep and
    /// measures line bouncing instead of misses.
    fn hostile_loop(buf: &[AtomicU64], seed: u64, dur: Duration, probe: &mut Probe) -> u64 {
        let start = Instant::now();
        let mut x = 0x9e37_79b9_7f4a_7c15u64 ^ seed.wrapping_mul(0x2545_f491_4f6c_dd1d);
        let mut iters = 0u64;
        while start.elapsed() < dur {
            for _ in 0..BATCH {
                x = x
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                buf[(x >> 20) as usize % buf.len()].fetch_add(1, Ordering::Relaxed);
            }
            iters += BATCH;
            probe.tick();
        }
        iters
    }

    /// Sequential reads over a buffer that fits in L1: no misses once it is warm.
    fn friendly_loop(dur: Duration, probe: &mut Probe) -> u64 {
        let buf: Vec<u64> = (0..FRIENDLY_BYTES / 8).map(|i| i as u64).collect();
        let start = Instant::now();
        let mut sum = 0u64;
        let mut iters = 0u64;
        while start.elapsed() < dur {
            for _ in 0..BATCH / 64 {
                for v in &buf {
                    sum = sum.wrapping_add(*v);
                }
            }
            std::hint::black_box(sum);
            iters += BATCH / 64 * buf.len() as u64;
            probe.tick();
        }
        iters
    }

    /// The cache-miss penalty (FreeBSD's cachemiss bench): `threads` hostile threads, each pinned to
    /// a cpu, thrashing the last-level cache beside one unpinned friendly thread that fits in L1.
    /// Reports what each side got, its misses and its penalty, then the behavioural checks: the
    /// friendly thread is never penalised, hostile ones are penalised within the cap, a penalty
    /// decays once the thread behaves, and sleeping preserves it.
    pub fn cache(threads: usize, secs: f64) {
        let steal0 = steal_ms();
        let k0 = KernelSnapshot::take();
        let cpus: Vec<u32> = cpu_infos().iter().map(|c| c.id).collect();
        let hostile = threads.max(1);
        // Non-zero fill: a zeroed allocation is never touched, and a first touch from every
        // hostile at once serialised on the object's page-table lock behind a preempted holder
        // at ~0.5 ms a fault, so the hostiles managed one batch each in 5 s.
        let buf: Arc<Vec<AtomicU64>> = Arc::new(
            (0..HOSTILE_BYTES / 8)
                .map(|i| AtomicU64::new(i as u64 | 1))
                .collect(),
        );
        let dur = Duration::from_secs_f64(secs);
        let buf2 = buf.clone();
        let res = run(hostile + 1, move |i, b| {
            let me = sys_thread_self_id();
            if i > 0 {
                let cpu = cpus[(i - 1) % cpus.len()];
                sys_thread_set_affinity(me, &CpuMask::single(cpu)).unwrap();
            }
            b.wait();
            let mut probe = Probe::start();
            let iters = if i == 0 {
                friendly_loop(dur, &mut probe)
            } else {
                hostile_loop(&buf, i as u64, dur, &mut probe)
            };
            let stats = thread_stats(me);
            (iters, stats, probe.finish())
        });
        let (friendly, hostiles) = res.split_first().unwrap();
        let hostile_iters: u64 = hostiles.iter().map(|r| r.0).sum();
        let penalties: Vec<u32> = hostiles.iter().map(|r| r.1.cache_penalty).collect();
        println!(
            "st cache threads={} hostile={} friendly_iters={} hostile_iters={} ratio={:.3} \
             friendly_penalty={} hostile_penalty min={} max={} friendly_misses={} hostile_misses={} \
             penalties=[{}] {} {}",
            threads,
            hostile,
            friendly.0,
            hostile_iters,
            friendly.0 as f64 / hostile_iters.max(1) as f64,
            friendly.1.cache_penalty,
            penalties.iter().min().unwrap(),
            penalties.iter().max().unwrap(),
            friendly.1.llc_misses,
            hostiles.iter().map(|r| r.1.llc_misses).sum::<u64>(),
            penalties
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(","),
            ProbeResult::fmt(&res.iter().map(|r| r.2).collect::<Vec<_>>()),
            kernel_delta(&k0, steal0)
        );

        // Decay: a thread that stops missing loses its penalty. Sleep-preserve: one that sleeps
        // keeps it, since it earned nothing back while asleep.
        let (after_hostile, after_friendly, before_sleep, after_sleep) =
            std::thread::spawn(move || {
                let me = sys_thread_self_id();
                let mut probe = Probe::start();
                let second = Duration::from_secs(1);
                hostile_loop(&buf2, 99, second, &mut probe);
                let after_hostile = thread_stats(me).cache_penalty;
                friendly_loop(second, &mut probe);
                let after_friendly = thread_stats(me).cache_penalty;
                hostile_loop(&buf2, 99, second, &mut probe);
                let before_sleep = thread_stats(me).cache_penalty;
                std::thread::sleep(Duration::from_millis(500));
                let after_sleep = thread_stats(me).cache_penalty;
                (after_hostile, after_friendly, before_sleep, after_sleep)
            })
            .join()
            .unwrap();
        println!(
            "st cache_checks threads={} friendly_zero={} hostile_bounded={} decay_zero={} \
             after_hostile={} after_friendly={} sleep_preserved={} before_sleep={} after_sleep={}",
            threads,
            friendly.1.cache_penalty == 0,
            penalties.iter().all(|p| *p > 0 && *p <= 48),
            after_friendly == 0,
            after_hostile,
            after_friendly,
            before_sleep == after_sleep,
            before_sleep,
            after_sleep,
        );
    }
}

fn main() {
    let opts = parse_args();
    let n = std::thread::available_parallelism().unwrap().get();
    println!(
        "st os={} cpus={} secs={} threads={:?} tests={:?} {}",
        std::env::consts::OS,
        n,
        opts.secs,
        opts.threads,
        opts.tests,
        cpu_summary()
    );
    for test in &opts.tests {
        for &t in &opts.threads {
            match test.as_str() {
                "spin" => spin(t, opts.secs),
                "yield" => yield_test(t, opts.secs),
                "pingpong" => pingpong(t, opts.secs),
                "sleeper" => sleeper(t, opts.secs),
                "memory" => memory(t, opts.secs / 4.0),
                "spawn" => spawn(t, opts.secs),
                #[cfg(target_os = "twizzler")]
                "cache" => cache_test::cache(t, opts.secs),
                other => panic!("unknown test {other}"),
            }
        }
    }
    println!("st done");
}

/// The per-OS sources of the counters: `thread_counters()` -> (switch-ins, migrations, cpu),
/// `KernelSnapshot` for the kernel-wide deltas, `steal_ms`, and `cpu_summary`.
#[cfg(target_os = "twizzler")]
mod os {
    use twizzler_abi::{
        object::ObjID,
        syscall::{
            sys_cpu_info, sys_info, sys_kernel_stats, sys_thread_read_stats, sys_thread_self_id,
            CacheKind, CpuInfo, CpuTopoLevelKind, KernelStats, ThreadSchedStats,
        },
    };

    pub fn thread_counters() -> (u64, u64, u32) {
        let mut s = ThreadSchedStats::default();
        sys_thread_read_stats(sys_thread_self_id(), &mut s).unwrap();
        (s.switches, s.migrations, s.cpu)
    }

    pub fn steal_ms() -> f64 {
        let info = sys_info();
        if info.version >= 2 { info.steal_ns as f64 / 1e6 } else { 0.0 }
    }

    pub struct KernelSnapshot(KernelStats);

    impl KernelSnapshot {
        pub fn take() -> Self {
            KernelSnapshot(sys_kernel_stats())
        }
    }

    /// Kernel-wide deltas since `k0` (the scheduler's switch/preempt/pick counters included),
    /// host steal since `steal0`, and the mean current cpu MHz.
    pub fn kernel_delta(k0: &KernelSnapshot, steal0: f64) -> String {
        let k0 = &k0.0;
        let k = sys_kernel_stats();
        let cpus = cpu_infos();
        let mhz = cpus.iter().map(|c| c.cur_mhz()).sum::<u64>() / cpus.len().max(1) as u64;
        macro_rules! d {
            ($f:ident) => {
                k.$f.wrapping_sub(k0.$f)
            };
        }
        format!(
            "k_ticks={} k_switches={} k_preempts={} k_steals={} steal_ms={:.1} mhz={} \
             k_sw_exit={} k_sw_block={} k_sw_yield={} k_sw_preempt={} k_sw_idle={} k_sw_to_idle={} \
             k_noop={} k_pre_slice={} k_pre_pri={} k_pick_pinned={} k_pick_warm={} k_pick_near={} \
             k_pick_far={} k_pick_cold={} k_pick_lowest={} k_pick_fallback={} k_pick_migrate={}",
            d!(hardticks),
            d!(ctx_switches),
            d!(preempts),
            d!(steals),
            steal_ms() - steal0,
            mhz,
            d!(switch_exit),
            d!(switch_block),
            d!(switch_yield),
            d!(switch_preempt),
            d!(switch_from_idle),
            d!(switch_to_idle),
            d!(resched_noop),
            d!(preempt_slice),
            d!(preempt_pri),
            d!(pick_pinned),
            d!(pick_last_warm),
            d!(pick_near),
            d!(pick_far),
            d!(pick_last_cold),
            d!(pick_lowest),
            d!(pick_fallback),
            d!(pick_migrate),
        )
    }

    pub fn thread_stats(id: ObjID) -> ThreadSchedStats {
        let mut s = ThreadSchedStats::default();
        sys_thread_read_stats(id, &mut s).unwrap();
        s
    }

    pub fn cpu_infos() -> Vec<CpuInfo> {
        (0..sys_info().cpu_count)
            .filter_map(|i| sys_cpu_info(i).ok())
            .collect()
    }

    /// Per-cpu MHz, the frequency source, the package/core/smt shape and the cache sizes.
    pub fn cpu_summary() -> String {
        let cpus = cpu_infos();
        let Some(first) = cpus.first() else {
            return String::new();
        };
        let distinct = |kind: CpuTopoLevelKind| {
            let mut ids: Vec<u32> = cpus
                .iter()
                .flat_map(|c| c.levels().iter().filter(|l| l.kind == kind).map(|l| l.id))
                .collect();
            ids.sort();
            ids.dedup();
            ids.len()
        };
        let cores = distinct(CpuTopoLevelKind::Core).max(1);
        let caches: Vec<String> = first
            .caches()
            .iter()
            .map(|c| {
                let k = match c.kind {
                    CacheKind::Data => "d",
                    CacheKind::Instruction => "i",
                    _ => "",
                };
                format!("L{}{}:{}", c.level, k, super::size_str(c.size))
            })
            .collect();
        format!(
            "mhz=[{}] nominal_mhz={} src={:?} topo={}pkg/{}core/{}smt caches={}",
            cpus.iter().map(|c| c.cur_mhz().to_string()).collect::<Vec<_>>().join(","),
            first.nominal_khz / 1000,
            first.freq_source,
            distinct(CpuTopoLevelKind::Package).max(1),
            cores,
            cpus.len() / cores,
            caches.join(",")
        )
    }
}

#[cfg(not(target_os = "twizzler"))]
mod os {
    use std::os::raw::{c_int, c_long};
    #[cfg(target_os = "freebsd")]
    use std::os::raw::c_void;

    #[repr(C)]
    struct Timeval {
        sec: c_long,
        usec: c_long,
    }

    /// Same layout on Linux and FreeBSD: two timevals, then fourteen longs ending in
    /// `nvcsw`, `nivcsw`.
    #[repr(C)]
    struct Rusage {
        utime: Timeval,
        stime: Timeval,
        longs: [c_long; 14],
    }

    const RUSAGE_THREAD: c_int = 1;

    unsafe extern "C" {
        fn getrusage(who: c_int, usage: *mut Rusage) -> c_int;
        fn sched_getcpu() -> c_int;
        #[cfg(target_os = "freebsd")]
        fn sysctlbyname(
            name: *const u8,
            oldp: *mut c_void,
            oldlenp: *mut usize,
            newp: *const c_void,
            newlen: usize,
        ) -> c_int;
    }

    fn rusage_switches() -> u64 {
        let mut r = Rusage {
            utime: Timeval { sec: 0, usec: 0 },
            stime: Timeval { sec: 0, usec: 0 },
            longs: [0; 14],
        };
        unsafe { getrusage(RUSAGE_THREAD, &mut r) };
        (r.longs[12] + r.longs[13]) as u64
    }

    #[cfg(target_os = "linux")]
    fn sched_field(name: &str) -> u64 {
        std::fs::read_to_string("/proc/thread-self/sched")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find(|l| l.starts_with(name))
                    .and_then(|l| l.rsplit(':').next())
                    .and_then(|v| v.trim().parse().ok())
            })
            .unwrap_or(0)
    }

    pub fn thread_counters() -> (u64, u64, u32) {
        #[cfg(target_os = "linux")]
        let migrations = sched_field("se.nr_migrations");
        #[cfg(not(target_os = "linux"))]
        let migrations = 0;
        (rusage_switches(), migrations, unsafe { sched_getcpu() } as u32)
    }

    #[cfg(target_os = "linux")]
    fn proc_stat() -> (u64, u64, f64) {
        let s = std::fs::read_to_string("/proc/stat").unwrap_or_default();
        let field = |key: &str| -> u64 {
            s.lines()
                .find(|l| l.starts_with(key))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        };
        // The aggregate cpu line's 8th column is steal, in USER_HZ (100) ticks.
        let steal = s
            .lines()
            .find(|l| l.starts_with("cpu "))
            .and_then(|l| l.split_whitespace().nth(8))
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
            * 10.0;
        (field("intr"), field("ctxt"), steal)
    }

    #[cfg(target_os = "freebsd")]
    fn sysctl_u64(name: &str) -> u64 {
        let cname = format!("{name}\0");
        let mut buf = [0u8; 8];
        let mut len = buf.len();
        let rc = unsafe {
            sysctlbyname(
                cname.as_ptr(),
                buf.as_mut_ptr() as *mut c_void,
                &mut len,
                std::ptr::null(),
                0,
            )
        };
        if rc != 0 {
            return 0;
        }
        match len {
            4 => u32::from_ne_bytes(buf[..4].try_into().unwrap()) as u64,
            _ => u64::from_ne_bytes(buf),
        }
    }

    pub fn steal_ms() -> f64 {
        #[cfg(target_os = "linux")]
        {
            proc_stat().2
        }
        #[cfg(not(target_os = "linux"))]
        {
            0.0
        }
    }

    pub struct KernelSnapshot {
        intr: u64,
        ctxt: u64,
    }

    impl KernelSnapshot {
        pub fn take() -> Self {
            #[cfg(target_os = "linux")]
            let (intr, ctxt, _) = proc_stat();
            #[cfg(target_os = "freebsd")]
            let (intr, ctxt) = (sysctl_u64("vm.stats.sys.v_intr"), sysctl_u64("vm.stats.sys.v_swtch"));
            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
            let (intr, ctxt) = (0, 0);
            KernelSnapshot { intr, ctxt }
        }
    }

    /// Interrupts stand in for ticks; preempts and steals are not exported by these kernels.
    pub fn kernel_delta(k0: &KernelSnapshot, steal0: f64) -> String {
        let k = KernelSnapshot::take();
        format!(
            "k_intr={} k_switches={} k_preempts=- k_steals=- steal_ms={:.1} mhz={}",
            k.intr.wrapping_sub(k0.intr),
            k.ctxt.wrapping_sub(k0.ctxt),
            steal_ms() - steal0,
            mhz_mean()
        )
    }

    fn mhz_list() -> Vec<u64> {
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string("/proc/cpuinfo")
                .unwrap_or_default()
                .lines()
                .filter(|l| l.starts_with("cpu MHz"))
                .filter_map(|l| l.rsplit(':').next()?.trim().parse::<f64>().ok())
                .map(|m| m as u64)
                .collect()
        }
        #[cfg(target_os = "freebsd")]
        {
            let n = sysctl_u64("hw.ncpu") as usize;
            let rate = sysctl_u64("hw.clockrate");
            (0..n)
                .map(|i| {
                    let f = sysctl_u64(&format!("dev.cpu.{i}.freq"));
                    if f != 0 { f } else { rate }
                })
                .collect()
        }
        #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
        {
            Vec::new()
        }
    }

    fn mhz_mean() -> u64 {
        let l = mhz_list();
        l.iter().sum::<u64>() / l.len().max(1) as u64
    }

    pub fn cpu_summary() -> String {
        let model = {
            #[cfg(target_os = "linux")]
            {
                std::fs::read_to_string("/proc/cpuinfo")
                    .unwrap_or_default()
                    .lines()
                    .find(|l| l.starts_with("model name"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|s| s.trim().replace(' ', "_"))
                    .unwrap_or_default()
            }
            #[cfg(target_os = "freebsd")]
            {
                let mut buf = vec![0u8; 256];
                let mut len = buf.len();
                unsafe {
                    sysctlbyname(
                        b"hw.model\0".as_ptr(),
                        buf.as_mut_ptr() as *mut c_void,
                        &mut len,
                        std::ptr::null(),
                        0,
                    )
                };
                String::from_utf8_lossy(&buf[..len.saturating_sub(1)]).replace(' ', "_")
            }
            #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
            {
                String::new()
            }
        };
        format!(
            "mhz=[{}] model={}",
            mhz_list().iter().map(|m| m.to_string()).collect::<Vec<_>>().join(","),
            model
        )
    }
}

#[cfg(target_os = "twizzler")]
fn size_str(bytes: u64) -> String {
    if bytes >= 1 << 20 { format!("{}M", bytes >> 20) } else { format!("{}K", bytes >> 10) }
}
