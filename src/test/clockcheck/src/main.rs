//! Checks the clock against an outside reference, and the cpu identification the kernel's
//! timekeeping and topology are derived from.
//!
//! `clockcheck [secs] [threads]`: dumps the CPUID leaves that matter, hammers `Instant::now()`
//! from `threads` threads for 3 s looking for a reading older than one another cpu already
//! published (once as-is, once with the read fenced behind the load it is compared with: the
//! runtime's `rdtsc` is not serializing, so unfenced it can execute while that load is still
//! waiting for the cache line), then prints one `CLK <i> <monotonic ns> <realtime ns> <tsc>` line a
//! second for `secs` seconds. A host timestamping those lines as they arrive gets the guest's clock
//! rate from the slope.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[cfg(target_arch = "x86_64")]
fn cpuid_dump() {
    use core::arch::x86_64::__cpuid_count;
    let leaf = |l: u32, s: u32| {
        let r = __cpuid_count(l, s);
        println!(
            "CPUID {:08x}.{} eax={:08x} ebx={:08x} ecx={:08x} edx={:08x}",
            l, s, r.eax, r.ebx, r.ecx, r.edx
        );
    };
    for l in [0, 1, 5, 6, 7, 0xa, 0xd, 0x15, 0x16] {
        leaf(l, 0);
    }
    leaf(0xd, 1);
    for s in 0..5 {
        leaf(4, s);
        leaf(0xb, s);
        leaf(0x8000_001d, s);
    }
    for l in [0x4000_0000, 0x4000_0001, 0x4000_0010] {
        leaf(l, 0);
    }
    for l in [
        0x8000_0000u32,
        0x8000_0001,
        0x8000_0005,
        0x8000_0006,
        0x8000_0007,
        0x8000_0008,
        0x8000_001e,
    ] {
        leaf(l, 0);
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn cpuid_dump() {}

#[cfg(target_arch = "x86_64")]
fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

#[cfg(not(target_arch = "x86_64"))]
fn tsc() -> u64 {
    0
}

#[cfg(target_os = "twizzler")]
fn clock_info() {
    use twizzler_abi::syscall::{ClockSource, ReadClockFlags, sys_read_clock_info};
    for (name, src) in [
        ("monotonic", ClockSource::BestMonotonic),
        ("realtime", ClockSource::BestRealTime),
    ] {
        match sys_read_clock_info(src, ReadClockFlags::empty()) {
            Ok(i) => println!(
                "CLOCKINFO {} tickrate_fs={} resolution_fs={}",
                name,
                i.tickrate().0,
                i.resolution().0
            ),
            Err(e) => println!("CLOCKINFO {} error {:?}", name, e),
        }
    }
}

#[cfg(not(target_os = "twizzler"))]
fn clock_info() {}

fn monotonicity(threads: usize, secs: u64, fenced: bool) {
    let base = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    let handles: Vec<_> = (0..threads)
        .map(|_| {
            let last = last.clone();
            std::thread::spawn(move || {
                let (mut reads, mut backwards, mut worst) = (0u64, 0u64, 0u64);
                while base.elapsed() < Duration::from_secs(secs) {
                    for _ in 0..1000 {
                        // Published before our read began, so ours must not be older.
                        let seen = last.load(Ordering::SeqCst);
                        #[cfg(target_arch = "x86_64")]
                        if fenced {
                            unsafe { core::arch::x86_64::_mm_lfence() };
                        }
                        let now = base.elapsed().as_nanos() as u64;
                        if now < seen {
                            backwards += 1;
                            worst = worst.max(seen - now);
                        }
                        last.fetch_max(now, Ordering::SeqCst);
                        reads += 1;
                    }
                }
                (reads, backwards, worst)
            })
        })
        .collect();
    let (mut reads, mut backwards, mut worst) = (0, 0, 0);
    for h in handles {
        let (r, b, w) = h.join().unwrap();
        reads += r;
        backwards += b;
        worst = worst.max(w);
    }
    println!(
        "MONOTONIC fenced={} threads={} reads={} backwards={} worst_ns={}",
        fenced, threads, reads, backwards, worst
    );
}

fn main() {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().and_then(|a| a.parse().ok()).unwrap_or(60);
    let threads: usize = args.next().and_then(|a| a.parse().ok()).unwrap_or(4);

    cpuid_dump();
    clock_info();
    println!(
        "PARALLELISM {}",
        std::thread::available_parallelism().map_or(0, |n| n.get())
    );
    monotonicity(threads, 3, false);
    monotonicity(threads, 3, true);

    let base = Instant::now();
    for i in 0..=secs {
        let target = Duration::from_secs(i);
        let now = base.elapsed();
        if now < target {
            std::thread::sleep(target - now);
        }
        let mono = base.elapsed().as_nanos();
        let real = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        println!("CLK {} {} {} {}", i, mono, real, tsc());
    }
    println!("CLOCKCHECK DONE");
}
