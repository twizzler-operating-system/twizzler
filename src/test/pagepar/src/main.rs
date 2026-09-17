//! Concurrent page-in workload.
//!
//! Reads many distinct files from several threads at once, so the pager sees more than one
//! page-data request in flight. The default test boot never does: `init` loads servers serially and
//! `unittest` runs its binaries one at a time, so `REQSTATS` reports a high-water mark of 1 and no
//! amount of pager thread topology can be evaluated against it.
//!
//! Usage: `pagepar [dir] [threads] [max_files] [wdir]`, defaults `/sysroot/lib`,
//! available_parallelism, 64, `/ext/pagepar-w`.
//!
//! Note the thread default: `xtask` does not pass `-smp` (the line is commented out in
//! `tools/xtask/src/qemu.rs`), so a bare `cargo xtask test`/`start-qemu` boot has one vCPU and
//! `available_parallelism` returns 1 -- every phase here then runs unthreaded while still
//! printing plausible numbers. Every line that depends on concurrency reports the vCPU count
//! next to the thread count for that reason. `many.py` passes `-smp` through explicitly.
//!
//! The write passes (create/write/close/rewrite/close2/unlink into `wdir`) run after the read
//! passes so the read numbers stay comparable with pre-write-bench runs. On Twizzler, dropping
//! a written file blocks on a full durable pager sync (`RawFile::shutdown` issues
//! `ObjectCmd::Sync` with `DURABLE|ASYNC_DURABLE`), so the close pass IS the durability
//! measurement; a Linux twin pairs it against `fsync`+`close`.

use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Barrier,
    },
    time::{Duration, Instant},
};

const BUF_BYTES: usize = 64 * 1024;

/// Worker threads lost to panics, summed across every phase.
///
/// This detects, it does not tolerate. A panic in pagepar is a bug in what pagepar is exercising,
/// and the fix belongs there rather than here. But the phases each rendezvous once, before any
/// work, so a panic after that point blocks nobody: the phase just collects with
/// `filter_map(|h| h.join().ok())` and means over whoever came back, and a 4-thread figure
/// computed from 3 threads reads exactly like a clean one. Anything non-zero here renames the
/// summary lines and makes the process exit non-zero, so that run cannot be read, greped or
/// scored as clean -- it names a bug to go fix.
///
/// The read/write passes need none of this: they rendezvous thirteen times, so a panic there
/// hangs instead, which is already unmissable.
static LOST_THREADS: AtomicU64 = AtomicU64::new(0);

/// Record, and announce, a phase that got back fewer threads than it started.
fn note_losses(phase: &str, got: usize, want: usize) {
    if got >= want {
        return;
    }
    LOST_THREADS.fetch_add((want - got) as u64, Ordering::Relaxed);
    println!(
        "pagepar: {} LOST {} of {} threads to panics; its figures cover the survivors only",
        phase,
        want - got,
        want
    );
}

/// Join workers that time themselves, separating a thread that returned no timing (an I/O error,
/// a legitimate skip) from one that panicked (a bug). Returns the timings and the skip count; the
/// panicked count is whatever is missing from both.
fn join_timed(handles: Vec<std::thread::JoinHandle<Option<Duration>>>) -> (Vec<Duration>, usize) {
    let mut timed = Vec::new();
    let mut skipped = 0;
    for h in handles {
        match h.join() {
            Ok(Some(d)) => timed.push(d),
            Ok(None) => skipped += 1,
            Err(_) => {}
        }
    }
    (timed, skipped)
}

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
    note_losses("ENUM", par.len(), nr_threads);
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
/// Each thread times its own loop, so the per-thread mean cannot on its own distinguish N threads
/// contending from N threads taking turns: under serialization every thread measures an
/// *uncontended* acquire and the mean reports that, which is how this phase once claimed a
/// 4-thread contended acquire cost 19 ns. The overlap ratio is what separates the two, so quote
/// the ratio, not the mean. Under full overlap `window == mean`; under full serialization
/// `window == mean x threads`.
///
/// The window is measured from the workers' own clocks (earliest start to latest finish). It used
/// to be timed by the parent around the joins, which with `nr_threads == vCPUs` can be descheduled
/// past the workers' start and report a window *shorter* than the loops it was meant to contain --
/// 0.55x on one thread, against a floor of 1.0, and 0 ns on another run.
///
/// A ratio near `nr_threads` still means the means are uncontended numbers, but read it against
/// the vCPU count before calling that a defect: on a one-vCPU guest N threads cannot overlap, and
/// full serialization is the correct answer rather than a measurement failure.
///
/// The question it was built for is still open: `futex_wake` now reports a real wake count, so
/// libstd's `RwLock` no longer wakes every reader on a write-unlock it could not confirm, and
/// nothing has measured what that changed.
fn lock_phase(nr_threads: usize, vcpus: usize) {
    use std::sync::{Mutex, RwLock};

    const ITERS: u32 = 20_000;
    /// One in this many acquisitions is a write. Read-mostly is the case rwlocks exist for.
    const WRITE_EVERY: u32 = 16;

    let mx = Arc::new((Mutex::new(0u64), RwLock::new(0u64)));
    let barrier = Arc::new(Barrier::new(nr_threads));

    let run = |use_rw: bool| {
        let mut handles = Vec::new();
        for _ in 0..nr_threads {
            let mx = mx.clone();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let start = Instant::now();
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
                let end = Instant::now();
                (start, end, (end - start) / ITERS)
            }));
        }
        let spans: Vec<(Instant, Instant, Duration)> =
            handles.into_iter().filter_map(|h| h.join().ok()).collect();
        note_losses(
            if use_rw {
                "LOCK(rwlock)"
            } else {
                "LOCK(mutex)"
            },
            spans.len(),
            nr_threads,
        );
        let window = match (
            spans.iter().map(|s| s.0).min(),
            spans.iter().map(|s| s.1).max(),
        ) {
            (Some(first), Some(last)) => (last - first) / ITERS,
            _ => Duration::default(),
        };
        let mut per: Vec<Duration> = spans.iter().map(|s| s.2).collect();
        per.sort();
        let mean = per
            .iter()
            .sum::<Duration>()
            .checked_div(per.len() as u32)
            .unwrap_or_default();
        (mean, window, per)
    };

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
        "pagepar: LOCK {} threads on {} vCPUs x {} acquires (1 write in {}): \
         mutex mean {} ns window {} ns (overlap {:.2}x of {}), \
         rwlock mean {} ns window {} ns (overlap {:.2}x)",
        nr_threads,
        vcpus,
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
        // Distinct files while threads <= files, so the contention measured is the fd table and
        // not one file's state; past that threads share and the figure mixes the two.
        let path = files[tid % files.len()].clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            read_loop(&path)
        }));
    }
    // A thread that returned `None` hit an I/O error, which is a legitimate skip; one that
    // panicked is a bug. Folding both into `filter_map` made a run over an unreadable file
    // indistinguishable from a run that lost a thread.
    let (par, unreadable) = join_timed(handles);
    note_losses("IO", par.len() + unreadable, nr_threads);
    if unreadable > 0 {
        println!(
            "pagepar: IO {} of {} threads could not read their file",
            unreadable, nr_threads
        );
    }
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

/// Time a small warm `write` on an already-open file, alone and contended -- the write-side
/// mirror of the IO phase. On Twizzler a 4 KiB overwrite of a resident page is a userspace
/// copy plus dirty tracking; no sync happens until close, which this phase does not time.
fn wio_phase(wdir: &Path, nr_threads: usize) {
    use std::io::{Seek, SeekFrom};

    const ITERS: u32 = 2048;
    const WRITE_BYTES: usize = 4096;

    fn write_loop(path: &Path) -> Option<Duration> {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .ok()?;
        let buf = [0x5au8; WRITE_BYTES];
        for _ in 0..64 {
            f.seek(SeekFrom::Start(0)).ok()?;
            f.write(&buf).ok()?;
        }
        let t = Instant::now();
        for _ in 0..ITERS {
            f.seek(SeekFrom::Start(0)).ok()?;
            std::hint::black_box(f.write(&buf).ok()?);
        }
        Some(t.elapsed() / ITERS)
    }

    let Some(solo) = write_loop(&wdir.join("wio_solo.bin")) else {
        println!(
            "pagepar: WIO create failed under {}, skipping",
            wdir.display()
        );
        return;
    };

    let barrier = Arc::new(Barrier::new(nr_threads));
    let mut handles = Vec::new();
    for tid in 0..nr_threads {
        let barrier = barrier.clone();
        let path = wdir.join(format!("wio_{}.bin", tid));
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            write_loop(&path)
        }));
    }
    let (par, unwritable) = join_timed(handles);
    note_losses("WIO", par.len() + unwritable, nr_threads);
    if unwritable > 0 {
        println!(
            "pagepar: WIO {} of {} threads could not write their file",
            unwritable, nr_threads
        );
    }

    // The read/write passes unlink what they create; these were not, so every run left another
    // set behind on a persistent disk.
    let _ = std::fs::remove_file(wdir.join("wio_solo.bin"));
    for tid in 0..nr_threads {
        let _ = std::fs::remove_file(wdir.join(format!("wio_{}.bin", tid)));
    }
    let par_max = par.iter().max().copied().unwrap_or_default();
    let par_mean = par
        .iter()
        .sum::<Duration>()
        .checked_div(par.len() as u32)
        .unwrap_or_default();

    println!(
        "pagepar: WIO {}-byte write+seek x {} iters: solo {} ns, {} threads mean {} ns max {} ns",
        WRITE_BYTES,
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
    note_losses("NAME", par.len(), nr_threads);
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
    // Kept separately from the thread count and reported alongside it: an explicit thread
    // argument on a one-vCPU boot produces threads that cannot overlap, which reads identically
    // to a measurement bug unless the vCPU count is on the line.
    let vcpus = std::thread::available_parallelism()
        .map(|p| p.get())
        .unwrap_or(4);
    let nr_threads = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(vcpus)
        .max(1);
    let max_files = args
        .next()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(64);
    let wdir =
        std::path::PathBuf::from(args.next().unwrap_or_else(|| "/ext/pagepar-w".to_string()));
    if let Err(e) = std::fs::create_dir_all(&wdir) {
        println!("pagepar: WDIR {} create failed: {}", wdir.display(), e);
    }

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
        // WIO and LOCK take no files. Returning here -- which is what this did -- let a bad `dir`
        // argument silently remove two phases that had nothing to do with it.
        println!(
            "pagepar: no files under {}; running the file-independent phases only",
            root
        );
        wio_phase(&wdir, nr_threads);
        lock_phase(nr_threads, vcpus);
        finish();
        return;
    }
    println!(
        "pagepar: {} files under {}, {} threads on {} vCPUs",
        files.len(),
        root,
        nr_threads,
        vcpus
    );

    name_phase(&files, nr_threads);
    io_phase(&files, nr_threads);
    wio_phase(&wdir, nr_threads);
    lock_phase(nr_threads, vcpus);

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
    //
    // A worker panicking between two of these hangs the run, because `Barrier` has no poisoning
    // and the party never fills again. That is deliberate: a panic here is a bug in what pagepar
    // is exercising, and a hang under `--kernel-arg=--diag` gets named by the kernel's thread
    // scan. Making the barrier survive a death only converts a loud failure into a completed run
    // with quietly short numbers.
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
        let wdir = wdir.clone();
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

            // Also records per-file byte counts: the write passes reuse them as the size
            // distribution, so writes mirror the reads without a stat round trip per file.
            let read_all = |open_files: &mut Vec<File>, buf: &mut [u8]| {
                let mut bytes = 0u64;
                let mut per_file = Vec::with_capacity(open_files.len());
                let t = Instant::now();
                for file in open_files.iter_mut() {
                    let mut this = 0u64;
                    loop {
                        match file.read(buf) {
                            Ok(0) => break,
                            Ok(n) => this += n as u64,
                            Err(_) => break,
                        }
                    }
                    bytes += this;
                    per_file.push(this);
                }
                (bytes, t.elapsed().as_nanos(), per_file)
            };

            // Spawned. Everything above this is thread startup, which the spawn phase measures.
            barrier.wait();

            // --- cold open ---
            let (mut open_files, cold_opens) = open_all(&files);
            barrier.wait();

            // --- cold read ---
            let count = open_files.len() as u64;
            let (cold_bytes, cold_read_ns, _) = read_all(&mut open_files, &mut buf);
            barrier.wait();
            // Held here while the main thread samples the fault counters. Without this the close
            // below runs concurrently with that sample and lands in the cold-pass count.
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
            let (warm_bytes, warm_read_ns, sizes) = read_all(&mut open_files, &mut buf);
            barrier.wait();
            // Same hold as after the cold read: the write phase must not start until the warm
            // fault count has been sampled.
            barrier.wait();

            // Patterned, not zeroed: the object store has zero-page fast paths, and a write
            // bench that hands it zeros measures those instead of the write path.
            for (i, b) in buf.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(31).wrapping_add(7);
            }

            // --- write: create + write ---
            let mut wpaths: Vec<(std::path::PathBuf, u64)> = Vec::new();
            let mut wfiles = Vec::new();
            let mut creates = Vec::new();
            let mut create_fails = 0u64;
            let mut write_ns = 0u128;
            let mut wbytes = 0u64;
            for (j, sz) in sizes.iter().enumerate() {
                let path = wdir.join(format!("pp{}_{}.bin", tid, j));
                let t0 = Instant::now();
                let f = File::create(&path);
                creates.push(t0.elapsed().as_nanos());
                match f {
                    Ok(mut f) => {
                        let t1 = Instant::now();
                        let mut left = *sz;
                        let mut ok = true;
                        while left > 0 {
                            let n = (left as usize).min(BUF_BYTES);
                            if f.write_all(&buf[..n]).is_err() {
                                ok = false;
                                break;
                            }
                            left -= n as u64;
                        }
                        write_ns += t1.elapsed().as_nanos();
                        if ok {
                            wbytes += *sz;
                        }
                        wpaths.push((path, *sz));
                        wfiles.push(f);
                    }
                    Err(_) => create_fails += 1,
                }
            }
            barrier.wait();

            // --- close: the durable sync, one blocking pager round trip per file ---
            let mut closes = Vec::new();
            for f in wfiles.drain(..) {
                let t0 = Instant::now();
                drop(f);
                closes.push(t0.elapsed().as_nanos());
            }
            barrier.wait();

            // --- rewrite: open existing + overwrite in place, no allocation or create ---
            let mut reopens = Vec::new();
            let mut rewrite_ns = 0u128;
            let mut wfiles2 = Vec::new();
            for (path, sz) in wpaths.iter() {
                let t0 = Instant::now();
                let f = std::fs::OpenOptions::new().write(true).open(path);
                reopens.push(t0.elapsed().as_nanos());
                if let Ok(mut f) = f {
                    let t1 = Instant::now();
                    let mut left = *sz;
                    while left > 0 {
                        let n = (left as usize).min(BUF_BYTES);
                        if f.write_all(&buf[..n]).is_err() {
                            break;
                        }
                        left -= n as u64;
                    }
                    rewrite_ns += t1.elapsed().as_nanos();
                    wfiles2.push(f);
                }
            }
            barrier.wait();

            // --- close2: sync the rewritten pages ---
            let mut closes2 = Vec::new();
            for f in wfiles2.drain(..) {
                let t0 = Instant::now();
                drop(f);
                closes2.push(t0.elapsed().as_nanos());
            }
            barrier.wait();

            // --- unlink ---
            let mut unlinks = Vec::new();
            for (path, _) in &wpaths {
                let t0 = Instant::now();
                let _ = std::fs::remove_file(path);
                unlinks.push(t0.elapsed().as_nanos());
            }
            barrier.wait();

            total_bytes.fetch_add(cold_bytes, Ordering::Relaxed);
            total_files.fetch_add(count, Ordering::Relaxed);
            Pass {
                count,
                cold_opens,
                cold_read_ns,
                warm_opens,
                warm_bytes,
                warm_read_ns,
                creates,
                create_fails,
                write_ns,
                wbytes,
                closes,
                reopens,
                rewrite_ns,
                closes2,
                unlinks,
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
    // Sampled above while the workers are parked on this second rendezvous. Releasing them and
    // then sampling -- which is what this did -- lets the close race the sample.
    barrier.wait();
    barrier.wait();
    mark("close end");
    mark("warm open start");
    barrier.wait();
    mark("warm open end");
    mark("warm read start");
    barrier.wait();
    mark("warm read end");
    // Bracket the write phases separately; without this the write-phase faults would land in
    // the "warm" fault count and silently change a pre-write-bench number. Same parked-workers
    // discipline as the cold sample above, for the same reason.
    let faults_after_warm = twizzler_abi::syscall::sys_memory_stats();
    barrier.wait();
    mark("wcw start");
    barrier.wait();
    mark("wcw end");
    mark("wclose start");
    barrier.wait();
    mark("wclose end");
    mark("rewrite start");
    barrier.wait();
    mark("rewrite end");
    mark("close2 start");
    barrier.wait();
    mark("close2 end");
    mark("unlink start");
    barrier.wait();
    mark("unlink end");

    // Aggregated across threads: the per-pass comparison is the point, and four threads' worth of
    // per-thread lines buries it.
    let mut cold_open_first = 0u128;
    let mut cold_open_rest = (0u128, 0u128);
    let mut warm_open_all = (0u128, 0u128);
    let mut cold_read_max = 0u128;
    let mut warm_read_max = 0u128;
    let mut warm_bytes = 0u64;
    let mut w = WAgg::default();
    for (i, h) in handles.into_iter().enumerate() {
        match h.join() {
            Ok(p) => {
                w.fold(i, &p);
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
            Err(_) => {
                LOST_THREADS.fetch_add(1, Ordering::Relaxed);
                println!("pagepar: thread {} panicked", i);
            }
        }
    }

    let faults_after = twizzler_abi::syscall::sys_memory_stats();
    let nr_faults = faults_after
        .page_fault_count
        .saturating_sub(faults_before.page_fault_count);
    let cold_faults = faults_after_cold
        .page_fault_count
        .saturating_sub(faults_before.page_fault_count);
    let warm_faults = faults_after_warm
        .page_fault_count
        .saturating_sub(faults_after_cold.page_fault_count);
    let write_faults = faults_after
        .page_fault_count
        .saturating_sub(faults_after_warm.page_fault_count);
    let bytes = total_bytes.load(Ordering::Relaxed);

    // Fail closed. Every aggregate below is summed over the threads that came back, so a run that
    // lost one reports a smaller workload as though that were the measurement. Renaming the tags
    // is what makes that unmissable: `PASSES`/`FAULTS`/`WPASSES` are what a reader's eye and a
    // downstream grep both key on, so a short run matches neither.
    let lost = LOST_THREADS.load(Ordering::Relaxed);
    let tag = |name: &'static str| if lost > 0 { "INCOMPLETE" } else { name };
    if lost > 0 {
        println!(
            "pagepar: INCOMPLETE {} thread(s) panicked; every figure below covers the survivors \
             only and is not comparable with a clean run",
            lost
        );
    }

    println!(
        "pagepar: {} {} files, {} KB in {} ms",
        tag("DONE"),
        total_files.load(Ordering::Relaxed),
        bytes / 1024,
        cold_elapsed.as_millis()
    );
    // The cold/warm split is what separates the pager from everything else: the same files, the
    // same opens and the same bytes, with the difference being that the second pass finds the
    // pages in core and the objects already known. Whatever survives into the warm pass is path
    // cost that no amount of paging work can remove.
    println!(
        "pagepar: {} cold open first {} us rest {} us mean / warm open {} us mean; \
         cold read max {} ms / warm read max {} ms; cold {} KB / warm {} KB; \
         faults cold {} / warm {}",
        tag("PASSES"),
        cold_open_first / 1000,
        cold_open_rest.0 / cold_open_rest.1.max(1) / 1000,
        warm_open_all.0 / warm_open_all.1.max(1) / 1000,
        cold_read_max / 1_000_000,
        warm_read_max / 1_000_000,
        bytes / 1024,
        warm_bytes / 1024,
        cold_faults,
        warm_faults,
    );
    println!(
        "pagepar: {} {} over the read phase ({} pages read, {:.2} faults/page); \
         mean {} us, max {} us; {} us of fault time vs {} us wall",
        tag("FAULTS"),
        cold_faults,
        bytes / 4096,
        cold_faults as f64 / ((bytes / 4096).max(1)) as f64,
        faults_after.page_fault_stats.mean.as_nanos() / 1000,
        faults_after.page_fault_stats.max.as_nanos() / 1000,
        (cold_faults as u128) * (faults_after.page_fault_stats.mean.as_nanos() / 1000),
        cold_elapsed.as_micros(),
    );
    // The close columns are the durability story: on Twizzler every close of a written file
    // is a blocking durable pager sync, so close mean x file count is the sync bill for the
    // whole write pass. The Linux twin pairs these against fsync+close.
    println!(
        "pagepar: {} create first {} us rest {} us mean / reopen {} us mean; \
         write max {} ms / rewrite max {} ms; close mean {} us max {} us / \
         close2 mean {} us max {} us; unlink mean {} us max {} us; \
         wrote {} KB x2, {} create fails; faults write-phases {} (total {})",
        tag("WPASSES"),
        w.create_first / 1000,
        w.create_rest.0 / w.create_rest.1.max(1) / 1000,
        w.reopens.0 / w.reopens.1.max(1) / 1000,
        w.write_max_ns / 1_000_000,
        w.rewrite_max_ns / 1_000_000,
        w.closes.0 / w.closes.1.max(1) / 1000,
        w.closes.2 / 1000,
        w.closes2.0 / w.closes2.1.max(1) / 1000,
        w.closes2.2 / 1000,
        w.unlinks.0 / w.unlinks.1.max(1) / 1000,
        w.unlinks.2 / 1000,
        w.wbytes / 1024,
        w.create_fails,
        write_faults,
        nr_faults,
    );

    finish();
}

/// Exit non-zero if any phase lost a thread.
///
/// `xtask test --autostart` reports the guest's exit code, so this is what stops a run that lost a
/// thread -- in a phase as much as in the passes -- from being recorded as a pass. Called last on
/// every path out of `main`, so the diagnostics are printed either way.
fn finish() {
    if LOST_THREADS.load(Ordering::Relaxed) > 0 {
        std::process::exit(1);
    }
}

/// One thread's read and write passes over its share of the files.
struct Pass {
    count: u64,
    cold_opens: Vec<u128>,
    cold_read_ns: u128,
    warm_opens: Vec<u128>,
    warm_bytes: u64,
    warm_read_ns: u128,
    creates: Vec<u128>,
    create_fails: u64,
    write_ns: u128,
    wbytes: u64,
    closes: Vec<u128>,
    reopens: Vec<u128>,
    rewrite_ns: u128,
    closes2: Vec<u128>,
    unlinks: Vec<u128>,
}

fn mean_us(v: &[u128]) -> u128 {
    v.iter().sum::<u128>() / (v.len().max(1) as u128) / 1000
}

fn max_us(v: &[u128]) -> u128 {
    v.iter().max().copied().unwrap_or(0) / 1000
}

/// Write-pass aggregation across threads, with the per-thread report line as a side effect.
#[derive(Default)]
struct WAgg {
    create_first: u128,
    create_rest: (u128, u128),
    reopens: (u128, u128),
    write_max_ns: u128,
    rewrite_max_ns: u128,
    closes: (u128, u128, u128),
    closes2: (u128, u128, u128),
    unlinks: (u128, u128, u128),
    wbytes: u64,
    create_fails: u64,
}

impl WAgg {
    fn fold(&mut self, i: usize, p: &Pass) {
        self.create_first = self
            .create_first
            .max(p.creates.first().copied().unwrap_or(0));
        self.create_rest.0 += p.creates.iter().skip(1).sum::<u128>();
        self.create_rest.1 += p.creates.len().saturating_sub(1) as u128;
        self.reopens.0 += p.reopens.iter().sum::<u128>();
        self.reopens.1 += p.reopens.len() as u128;
        self.write_max_ns = self.write_max_ns.max(p.write_ns);
        self.rewrite_max_ns = self.rewrite_max_ns.max(p.rewrite_ns);
        for (agg, v) in [
            (&mut self.closes, &p.closes),
            (&mut self.closes2, &p.closes2),
            (&mut self.unlinks, &p.unlinks),
        ] {
            agg.0 += v.iter().sum::<u128>();
            agg.1 += v.len() as u128;
            agg.2 = agg.2.max(v.iter().max().copied().unwrap_or(0));
        }
        self.wbytes += p.wbytes;
        self.create_fails += p.create_fails;
        println!(
            "pagepar: wthread {} {} files ({} create fails); create first {} us rest {} us mean, \
             write {} ms; close mean {} us max {} us; reopen {} us mean, rewrite {} ms; \
             close2 mean {} us; unlink mean {} us",
            i,
            p.creates.len(),
            p.create_fails,
            p.creates.first().copied().unwrap_or(0) / 1000,
            p.creates.iter().skip(1).sum::<u128>()
                / (p.creates.len().saturating_sub(1).max(1) as u128)
                / 1000,
            p.write_ns / 1_000_000,
            mean_us(&p.closes),
            max_us(&p.closes),
            mean_us(&p.reopens),
            p.rewrite_ns / 1_000_000,
            mean_us(&p.closes2),
            mean_us(&p.unlinks),
        );
    }
}
