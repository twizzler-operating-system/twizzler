//! Concurrent page-in workload.
//!
//! Reads many distinct files from several threads at once, so the pager sees more than one
//! page-data request in flight. The default test boot never does: `init` loads servers serially and
//! `unittest` runs its binaries one at a time, so `REQSTATS` reports a high-water mark of 1 and no
//! amount of pager thread topology can be evaluated against it.
//!
//! Usage: `pagepar [dir] [threads] [max_files]`, defaults `/sysroot/lib`, available_parallelism,
//! 2048.

use std::{
    fs::File,
    io::Read,
    path::PathBuf,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

const BUF_BYTES: usize = 64 * 1024;

/// Collect files under `root`, in discovery order.
///
/// Deliberately does not stat: `metadata()` is a naming round trip per file in the guest, and
/// sorting ~2000 of them by size took longer than the reads it was meant to order.
///
/// Size matters, so the *default root* supplies it instead. The pager routes a page-data request to
/// a reserved fast lane when it is at most `FAST_PAGE_LIMIT` (16) pages, and that lane is only
/// `MAX_FAST_LANES` (2) threads wide -- so a workload of small files pins concurrency at 2 however
/// many threads read it, which is exactly what the first version of this measured. `/sysroot/lib`
/// holds a handful of multi-MB objects, whose requests clear the limit and land on the bulk lane.
fn collect_files(root: &str, max: usize) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut dirs = vec![PathBuf::from(root)];
    while let Some(dir) = dirs.pop() {
        if files.len() >= max {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match entry.file_type() {
                Ok(ft) if ft.is_dir() => dirs.push(path),
                Ok(ft) if ft.is_file() && files.len() < max => files.push(path),
                _ => {}
            }
        }
    }
    files
}

/// Time `read_dir` over `root` itself: cold once, then warm, then warm from several threads.
///
/// Enumeration is a separate cost from open and has never been measured. It is one naming gate call
/// per 128 entries (libstd's `ReadDir` buffer), but the server side may go all the way to the pager
/// and pay a further gate call per symlink, so the cold and warm numbers are expected to differ by
/// orders of magnitude rather than by a constant.
fn enum_phase(root: &str, nr_threads: usize) {
    fn count_dir(dir: &str) -> usize {
        std::fs::read_dir(dir)
            .map(|d| d.flatten().count())
            .unwrap_or(0)
    }

    let t_cold = Instant::now();
    let n = count_dir(root);
    let cold = t_cold.elapsed();
    if n == 0 {
        println!("pagepar: ENUM {} is empty, skipping", root);
        return;
    }

    const WARM_ITERS: usize = 8;
    let t_warm = Instant::now();
    for _ in 0..WARM_ITERS {
        std::hint::black_box(count_dir(root));
    }
    let warm = t_warm.elapsed() / WARM_ITERS as u32;

    let barrier = Arc::new(Barrier::new(nr_threads));
    let mut handles = Vec::new();
    for _ in 0..nr_threads {
        let barrier = barrier.clone();
        let root = root.to_string();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let t = Instant::now();
            for _ in 0..WARM_ITERS {
                std::hint::black_box(count_dir(&root));
            }
            t.elapsed() / WARM_ITERS as u32
        }));
    }
    let par: Vec<_> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    let par_max = par.iter().max().copied().unwrap_or_default();

    println!(
        "pagepar: ENUM {} ({} entries): cold {} us, warm {} us ({} us/entry), \
         {} threads warm max {} us",
        root,
        n,
        cold.as_micros(),
        warm.as_micros(),
        warm.as_micros() / n as u128,
        nr_threads,
        par_max.as_micros(),
    );
}

/// Compare `Mutex` against `RwLock` under a read-mostly load, both from libstd.
///
/// **This phase does not measure contended lock cost, and no number it prints should be quoted as
/// one.** It is kept because the shape of its output diagnoses *why* -- see the overlap ratio below
/// -- and because that failure generalizes to any contention benchmark run in a guest. Read
/// `stdperf.md` item 1 before using it for anything.
///
/// Two independent defects, both found only after its numbers had been published and retracted:
///
/// - Each thread times its own loop and the phase reports the mean of those, which cannot
///   distinguish N threads contending from N threads taking turns. Measured overlap is 1.5-3.0x of
///   `nr_threads`, so every thread spends much of its loop on an *uncontended* lock and the mean
///   says so, confidently and wrongly.
/// - The window that was meant to correct this is timed by the parent, which with `nr_threads ==
///   vCPUs` can be descheduled until the workers finish. One run reported a window of 0 ns.
///
/// Fixing it properly means fewer workers than the guest has vCPUs, or computing the aggregate
/// inside the workers rather than around them.
///
/// The question it was built for is still open: `futex_wake` now reports a real wake count, so
/// libstd's `RwLock` no longer wakes every reader on a write-unlock it could not confirm, and
/// nothing has measured what that changed.
fn lock_phase(nr_threads: usize) {
    use std::sync::{Mutex, RwLock};

    const ITERS: u32 = 20_000;
    /// One in this many acquisitions is a write. Read-mostly is the case rwlocks exist for.
    const WRITE_EVERY: u32 = 16;

    let mx = Arc::new((Mutex::new(0u64), RwLock::new(0u64)));
    // nr_threads + 1: the parent joins the barrier so it can time the whole window from one clock.
    let barrier = Arc::new(Barrier::new(nr_threads + 1));

    let run = |use_rw: bool| {
        let mut handles = Vec::new();
        for _ in 0..nr_threads {
            let mx = mx.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let t = Instant::now();
                for i in 0..ITERS {
                    let write = i % WRITE_EVERY == 0;
                    if use_rw {
                        if write {
                            *mx.1.write().unwrap() += 1;
                        } else {
                            std::hint::black_box(*mx.1.read().unwrap());
                        }
                    } else {
                        let mut g = mx.0.lock().unwrap();
                        if write {
                            *g += 1;
                        } else {
                            std::hint::black_box(*g);
                        }
                    }
                }
                t.elapsed() / ITERS
            }));
        }
        barrier.wait();
        let window = Instant::now();
        let mut v: Vec<Duration> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
        let window = window.elapsed() / ITERS;
        v.sort();
        let mean = v
            .iter()
            .sum::<Duration>()
            .checked_div(v.len() as u32)
            .unwrap_or_default();
        (mean, window, v)
    };

    // Report the window alongside the per-thread mean, because the two together are the only way
    // to tell contention from its opposite. Each thread times its *own* loop, so if the threads
    // serialize instead of overlapping, every one of them measures an *uncontended* lock and the
    // mean reports that -- which is how this phase once claimed a 4-thread contended acquire cost
    // 19 ns. Under full overlap window == mean; under full serialization window == mean x threads.
    // Quote the ratio, not the mean.
    let (m_mean, m_window, m_all) = run(false);
    let (r_mean, r_window, r_all) = run(true);
    let ratio = |w: Duration, m: Duration| {
        if m.as_nanos() == 0 {
            0.0
        } else {
            w.as_nanos() as f64 / m.as_nanos() as f64
        }
    };
    println!(
        "pagepar: LOCK {} threads x {} acquires (1 write in {}): \
         mutex mean {} ns window {} ns (overlap {:.2}x of {}), \
         rwlock mean {} ns window {} ns (overlap {:.2}x)",
        nr_threads,
        ITERS,
        WRITE_EVERY,
        m_mean.as_nanos(),
        m_window.as_nanos(),
        ratio(m_window, m_mean),
        nr_threads,
        r_mean.as_nanos(),
        r_window.as_nanos(),
        ratio(r_window, r_mean),
    );
    println!(
        "pagepar: LOCK per-thread mutex {:?} rwlock {:?}",
        m_all.iter().map(|d| d.as_nanos()).collect::<Vec<_>>(),
        r_all.iter().map(|d| d.as_nanos()).collect::<Vec<_>>(),
    );
}

/// Time a small warm `read` on an already-open file, alone.
///
/// Every `read`/`write`/`seek` in the process goes through the runtime's one global fd table, so
/// this asks what a read costs when the pages are already resident and nothing is being faulted --
/// i.e. the fd path itself rather than the pager. Each thread has its own file and re-reads the
/// first page of it, so the only thing the threads share is that table.
fn io_phase(files: &[PathBuf], nr_threads: usize) {
    use std::io::{Read, Seek, SeekFrom};

    const ITERS: u32 = 2048;
    const READ_BYTES: usize = 4096;

    fn read_loop(path: &PathBuf) -> Option<Duration> {
        let mut f = File::open(path).ok()?;
        let mut buf = [0u8; READ_BYTES];
        // Warm: first touch of each page is a fault, which is not what this measures.
        for _ in 0..64 {
            f.seek(SeekFrom::Start(0)).ok()?;
            f.read(&mut buf).ok()?;
        }
        let t = Instant::now();
        for _ in 0..ITERS {
            f.seek(SeekFrom::Start(0)).ok()?;
            std::hint::black_box(f.read(&mut buf).ok()?);
        }
        Some(t.elapsed() / ITERS)
    }

    let Some(solo) = files.first().and_then(read_loop) else {
        println!("pagepar: IO no file readable, skipping");
        return;
    };

    let barrier = Arc::new(Barrier::new(nr_threads));
    let mut handles = Vec::new();
    for tid in 0..nr_threads {
        let barrier = barrier.clone();
        // Distinct files, so the contention measured is the fd table and not one file's state.
        let path = files[tid % files.len()].clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            read_loop(&path)
        }));
    }
    let par: Vec<Duration> = handles
        .into_iter()
        .filter_map(|h| h.join().ok().flatten())
        .collect();
    let par_max = par.iter().max().copied().unwrap_or_default();
    let par_mean = par
        .iter()
        .sum::<Duration>()
        .checked_div(par.len() as u32)
        .unwrap_or_default();

    println!(
        "pagepar: IO {}-byte read+seek x {} iters: solo {} ns, {} threads mean {} ns max {} ns",
        READ_BYTES,
        ITERS,
        solo.as_nanos(),
        nr_threads,
        par_mean.as_nanos(),
        par_max.as_nanos(),
    );
}

/// Time a warm naming lookup with nothing else attached to it.
///
/// `twz_rt_resolve_name` is one naming `get` gate call and nothing more -- no object map, no
/// meta-page fault, none of the rest of what `File::open` does. It is therefore the only figure in
/// this program that is purely the naming service: client wrapper, compartment transition, and
/// `namei`. Read it, not the open phase, when the question is whether naming got faster.
///
/// Paths are cycled rather than repeated, so a per-path memo in the client would show up as an
/// implausible number rather than as a win.
fn name_phase(files: &[PathBuf], nr_threads: usize) {
    const ITERS: u32 = 512;
    const NR_PATHS: usize = 16;

    let paths: Vec<String> = files
        .iter()
        .take(NR_PATHS)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    if paths.is_empty() {
        return;
    }

    fn resolve_loop(paths: &[String]) -> (Duration, u32) {
        let mut ok = 0;
        let t = Instant::now();
        for i in 0..ITERS {
            let p = &paths[i as usize % paths.len()];
            if twizzler_rt_abi::fd::twz_rt_resolve_name(Default::default(), p).is_ok() {
                ok += 1;
            }
        }
        (t.elapsed() / ITERS, ok)
    }

    // Warm the server's caches first; a cold external-namespace lookup is a pager round trip and
    // has nothing to do with the steady-state cost this measures.
    let (_, ok) = resolve_loop(&paths);
    if ok == 0 {
        println!("pagepar: NAME no path resolved, skipping");
        return;
    }
    let (warm, _) = resolve_loop(&paths);

    let barrier = Arc::new(Barrier::new(nr_threads));
    let paths = Arc::new(paths);
    let mut handles = Vec::new();
    for _ in 0..nr_threads {
        let barrier = barrier.clone();
        let paths = paths.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            resolve_loop(&paths).0
        }));
    }
    let par: Vec<Duration> = handles.into_iter().filter_map(|h| h.join().ok()).collect();
    let par_max = par.iter().max().copied().unwrap_or_default();
    let par_mean = par
        .iter()
        .sum::<Duration>()
        .checked_div(par.len() as u32)
        .unwrap_or_default();

    println!(
        "pagepar: NAME {} paths x {} iters: warm {} ns, {} threads mean {} ns max {} ns",
        paths.len(),
        ITERS,
        warm.as_nanos(),
        nr_threads,
        par_mean.as_nanos(),
        par_max.as_nanos(),
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "/sysroot/lib".to_string());
    let nr_threads = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(4)
        })
        .max(1);
    let max_files = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(64);

    // Syscall floor. A warm cross-compartment gate call makes eight syscalls (frame's
    // active-sctx read, the callee's settls/sctx_attach/set-active-sctx/self-id/settls, and
    // restore_frame's settls/set-active-sctx), so this sets how much of a ~200us gate call is
    // just kernel round trips before anything else is blamed.
    const SYSCALL_ITERS: u32 = 10_000;
    let t_sys = Instant::now();
    for _ in 0..SYSCALL_ITERS {
        std::hint::black_box(twizzler_abi::syscall::sys_thread_self_id());
    }
    println!(
        "pagepar: SYSCALL {} ns per sys_thread_self_id ({} iters)",
        t_sys.elapsed().as_nanos() / SYSCALL_ITERS as u128,
        SYSCALL_ITERS,
    );

    enum_phase(&root, nr_threads);

    let files = collect_files(&root, max_files);
    if files.is_empty() {
        println!("pagepar: no files under {}", root);
        return;
    }
    println!(
        "pagepar: {} files under {}, {} threads",
        files.len(),
        root,
        nr_threads
    );

    name_phase(&files, nr_threads);
    io_phase(&files, nr_threads);
    lock_phase(nr_threads);

    // Striped, not a shared cursor. A cursor looks fairer but is not: thread spawn is slow enough
    // in the guest that the first threads to start drain it before the last ones exist, and a
    // measurement of concurrency taken with two of four threads running is worthless. Striping
    // hands each thread its share up front, and the barriers make each phase start together.
    //
    // Phases are separated rather than interleaved because open and read have nothing in common.
    // Interleaved, every per-open figure was a mean over a distribution that turned out to be 25%
    // cold calls carrying 99% of the time, and ~60% of the "read phase" was actually thread spawn
    // -- neither of which is visible until the phases are cut apart. Each boundary is stamped in
    // absolute monotonic microseconds, the same base the runtime's counter records carry, so a
    // record can be attributed to the phase it happened in.
    //
    // The cost of this shape is that every file stays open across the read phase: one mapped
    // object per file, against 2^17 address-space slots, so a few thousand files is fine.
    let files = Arc::new(files);
    // nr_threads + 1: the main thread joins each barrier so it can stamp the boundary from one
    // clock, rather than each worker reporting its own idea of when a phase began.
    let barrier = Arc::new(Barrier::new(nr_threads + 1));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let total_files = Arc::new(AtomicU64::new(0));

    // Page faults, bracketing the read phase. A major fault is one page-data request to the pager;
    // a minor one is a page already in core that just needs mapping into this context. The pager
    // can be nearly idle while the kernel spends the whole wall clock on the latter, and only this
    // separates those two worlds.
    let faults_before = twizzler_abi::syscall::sys_memory_stats();

    let mark = |name: &str| {
        println!(
            "pagepar: PHASE {} {} us",
            name,
            twizzler_rt_abi::time::twz_rt_get_monotonic_time().as_micros()
        );
    };

    let start = Instant::now();
    mark("spawn start");
    let mut handles = Vec::new();
    for tid in 0..nr_threads {
        let files = files.clone();
        let barrier = barrier.clone();
        let total_bytes = total_bytes.clone();
        let total_files = total_files.clone();
        handles.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; BUF_BYTES];

            // Open every file this thread owns, timing each open individually. Kept individually
            // rather than summed: the first open on a thread is its first entry into the naming
            // and monitor compartments and costs milliseconds, while the rest cost tens of
            // microseconds. A sum reports neither.
            let open_all = |files: &Vec<PathBuf>| {
                let mut open_files = Vec::new();
                let mut opens = Vec::new();
                let mut idx = tid;
                while let Some(path) = files.get(idx) {
                    idx += nr_threads;
                    let t0 = Instant::now();
                    let opened = File::open(path);
                    opens.push(t0.elapsed().as_nanos());
                    if let Ok(file) = opened {
                        open_files.push(file);
                    }
                }
                (open_files, opens)
            };

            let read_all = |open_files: &mut Vec<File>, buf: &mut [u8]| {
                let mut bytes = 0u64;
                let t = Instant::now();
                for file in open_files.iter_mut() {
                    loop {
                        match file.read(buf) {
                            Ok(0) => break,
                            Ok(n) => bytes += n as u64,
                            Err(_) => break,
                        }
                    }
                }
                (bytes, t.elapsed().as_nanos())
            };

            // Spawned. Everything above this is thread startup, which the spawn phase measures.
            barrier.wait();

            // --- cold open ---
            let (mut open_files, cold_opens) = open_all(&files);
            barrier.wait();

            // --- cold read ---
            let count = open_files.len() as u64;
            let (cold_bytes, cold_read_ns) = read_all(&mut open_files, &mut buf);
            barrier.wait();

            // Closed before the warm pass, so the warm open is a real open -- naming lookup,
            // monitor gate and object map -- against an object the runtime and kernel have already
            // seen, rather than a no-op on a handle still held.
            drop(open_files);
            barrier.wait();

            // --- warm open ---
            let (mut open_files, warm_opens) = open_all(&files);
            barrier.wait();

            // --- warm read ---
            let (warm_bytes, warm_read_ns) = read_all(&mut open_files, &mut buf);
            barrier.wait();

            total_bytes.fetch_add(cold_bytes, Ordering::Relaxed);
            total_files.fetch_add(count, Ordering::Relaxed);
            Pass {
                count,
                cold_opens,
                cold_bytes,
                cold_read_ns,
                warm_opens,
                warm_bytes,
                warm_read_ns,
            }
        }));
    }

    // Each of these releases only once every worker has arrived, so the stamp either side of it
    // bounds a phase in which every thread was inside that phase.
    barrier.wait();
    mark("spawn end");
    mark("open start");
    barrier.wait();
    mark("open end");
    mark("read start");
    barrier.wait();
    mark("read end");
    let faults_after_cold = twizzler_abi::syscall::sys_memory_stats();
    let cold_elapsed = start.elapsed();
    barrier.wait();
    mark("close end");
    mark("warm open start");
    barrier.wait();
    mark("warm open end");
    mark("warm read start");
    barrier.wait();
    mark("warm read end");

    // Aggregated across threads: the per-pass comparison is the point, and four threads' worth of
    // per-thread lines buries it.
    let mut cold_open_first = 0u128;
    let mut cold_open_rest = (0u128, 0u128);
    let mut warm_open_all = (0u128, 0u128);
    let mut cold_read_max = 0u128;
    let mut warm_read_max = 0u128;
    let mut warm_bytes = 0u64;
    for (i, h) in handles.into_iter().enumerate() {
        match h.join() {
            Ok(p) => {
                // The first open on a thread is its first entry into the naming and monitor
                // compartments and costs milliseconds; averaging it in hides both it and the rest.
                cold_open_first = cold_open_first.max(p.cold_opens.first().copied().unwrap_or(0));
                cold_open_rest.0 += p.cold_opens.iter().skip(1).sum::<u128>();
                cold_open_rest.1 += p.cold_opens.len().saturating_sub(1) as u128;
                warm_open_all.0 += p.warm_opens.iter().sum::<u128>();
                warm_open_all.1 += p.warm_opens.len() as u128;
                cold_read_max = cold_read_max.max(p.cold_read_ns);
                warm_read_max = warm_read_max.max(p.warm_read_ns);
                warm_bytes += p.warm_bytes;
                println!(
                    "pagepar: thread {} {} files; cold open first {} us rest {} us mean, read {} ms; \
                     warm open {} us mean, read {} ms",
                    i,
                    p.count,
                    p.cold_opens.first().copied().unwrap_or(0) / 1000,
                    p.cold_opens.iter().skip(1).sum::<u128>()
                        / (p.cold_opens.len().saturating_sub(1).max(1) as u128)
                        / 1000,
                    p.cold_read_ns / 1_000_000,
                    p.warm_opens.iter().sum::<u128>() / (p.warm_opens.len().max(1) as u128) / 1000,
                    p.warm_read_ns / 1_000_000,
                );
            }
            Err(_) => println!("pagepar: thread {} panicked", i),
        }
    }

    let faults_after = twizzler_abi::syscall::sys_memory_stats();
    let nr_faults = faults_after
        .page_fault_count
        .saturating_sub(faults_before.page_fault_count);
    let cold_faults = faults_after_cold
        .page_fault_count
        .saturating_sub(faults_before.page_fault_count);
    let bytes = total_bytes.load(Ordering::Relaxed);

    println!(
        "pagepar: DONE {} files, {} KB in {} ms",
        total_files.load(Ordering::Relaxed),
        bytes / 1024,
        cold_elapsed.as_millis()
    );
    // The cold/warm split is what separates the pager from everything else: the same files, the
    // same opens and the same bytes, with the difference being that the second pass finds the
    // pages in core and the objects already known. Whatever survives into the warm pass is path
    // cost that no amount of paging work can remove.
    println!(
        "pagepar: PASSES cold open first {} us rest {} us mean / warm open {} us mean; \
         cold read max {} ms / warm read max {} ms; cold {} KB / warm {} KB; \
         faults cold {} / warm {}",
        cold_open_first / 1000,
        cold_open_rest.0 / cold_open_rest.1.max(1) / 1000,
        warm_open_all.0 / warm_open_all.1.max(1) / 1000,
        cold_read_max / 1_000_000,
        warm_read_max / 1_000_000,
        bytes / 1024,
        warm_bytes / 1024,
        cold_faults,
        nr_faults.saturating_sub(cold_faults),
    );
    println!(
        "pagepar: FAULTS {} over the read phase ({} pages read, {:.2} faults/page); \
         mean {} us, max {} us; {} us of fault time vs {} us wall",
        cold_faults,
        bytes / 4096,
        cold_faults as f64 / ((bytes / 4096).max(1)) as f64,
        faults_after.page_fault_stats.mean.as_nanos() / 1000,
        faults_after.page_fault_stats.max.as_nanos() / 1000,
        (cold_faults as u128) * (faults_after.page_fault_stats.mean.as_nanos() / 1000),
        cold_elapsed.as_micros(),
    );
}

/// One thread's two passes over its share of the files.
struct Pass {
    count: u64,
    cold_opens: Vec<u128>,
    cold_bytes: u64,
    cold_read_ns: u128,
    warm_opens: Vec<u128>,
    warm_bytes: u64,
    warm_read_ns: u128,
}
