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
    time::Instant,
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

    // Striped, not a shared cursor. A cursor looks fairer but is not: thread spawn is slow enough
    // in the guest that the first threads to start drain it before the last ones exist, and a
    // measurement of concurrency taken with two of four threads running is worthless. Striping
    // hands each thread its share up front, and the barrier makes them start together -- which is
    // the entire point of the workload.
    let files = Arc::new(files);
    let barrier = Arc::new(Barrier::new(nr_threads));
    let total_bytes = Arc::new(AtomicU64::new(0));
    let total_files = Arc::new(AtomicU64::new(0));

    // Page faults, bracketing the read phase. A major fault is one page-data request to the pager;
    // a minor one is a page already in core that just needs mapping into this context. The pager
    // can be nearly idle while the kernel spends the whole wall clock on the latter, and only this
    // separates those two worlds.
    let faults_before = twizzler_abi::syscall::sys_memory_stats();

    let start = Instant::now();
    let mut handles = Vec::new();
    for tid in 0..nr_threads {
        let files = files.clone();
        let barrier = barrier.clone();
        let total_bytes = total_bytes.clone();
        let total_files = total_files.clone();
        handles.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; BUF_BYTES];
            let mut bytes = 0u64;
            let mut count = 0u64;
            // Split open from read. `RawFile::read` is a memcpy out of a mapped object, so if the
            // wall clock is not in the copy or in faults it has to be in the open -- a naming gate
            // call plus a monitor map plus the meta-page fault, none of which is paging.
            let mut open_ns = 0u128;
            let mut read_ns = 0u128;
            barrier.wait();
            let mut idx = tid;
            loop {
                let Some(path) = files.get(idx) else {
                    break;
                };
                idx += nr_threads;
                let t0 = Instant::now();
                let opened = File::open(path);
                open_ns += t0.elapsed().as_nanos();
                let Ok(mut file) = opened else {
                    continue;
                };
                count += 1;
                let t1 = Instant::now();
                loop {
                    match file.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => bytes += n as u64,
                        Err(_) => break,
                    }
                }
                read_ns += t1.elapsed().as_nanos();
            }
            total_bytes.fetch_add(bytes, Ordering::Relaxed);
            total_files.fetch_add(count, Ordering::Relaxed);
            (count, bytes, open_ns, read_ns)
        }));
    }

    for (i, h) in handles.into_iter().enumerate() {
        match h.join() {
            Ok((count, bytes, open_ns, read_ns)) => println!(
                "pagepar: thread {} read {} files, {} KB; open {} ms, read {} ms",
                i,
                count,
                bytes / 1024,
                open_ns / 1_000_000,
                read_ns / 1_000_000
            ),
            Err(_) => println!("pagepar: thread {} panicked", i),
        }
    }

    let elapsed = start.elapsed();
    let faults_after = twizzler_abi::syscall::sys_memory_stats();
    let nr_faults = faults_after
        .page_fault_count
        .saturating_sub(faults_before.page_fault_count);
    let bytes = total_bytes.load(Ordering::Relaxed);

    println!(
        "pagepar: DONE {} files, {} KB in {} ms",
        total_files.load(Ordering::Relaxed),
        bytes / 1024,
        elapsed.as_millis()
    );
    println!(
        "pagepar: FAULTS {} over the read phase ({} pages read, {:.2} faults/page); \
         mean {} us, max {} us; {} us of fault time vs {} us wall",
        nr_faults,
        bytes / 4096,
        nr_faults as f64 / ((bytes / 4096).max(1)) as f64,
        faults_after.page_fault_stats.mean.as_nanos() / 1000,
        faults_after.page_fault_stats.max.as_nanos() / 1000,
        (nr_faults as u128) * (faults_after.page_fault_stats.mean.as_nanos() / 1000),
        elapsed.as_micros(),
    );
}
