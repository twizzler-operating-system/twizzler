//! Benchmarks and measurement reports for the raw queue.
//!
//! Deliberately a separate target with `test = false` (see `Cargo.toml`): these take seconds, and
//! `cargo test` should stay fast enough to run on every edit. The unit tests in `src/lib.rs` cover
//! correctness, including the concurrent producer/consumer path — just at a size that finishes in
//! milliseconds.
//!
//! ```text
//! cargo bench -p twizzler-queue-raw                    # the #[bench] functions
//! # the reports are #[test]s, which `cargo bench` skips; name the target to run them:
//! cargo test --release -p twizzler-queue-raw --bench queue -- --nocapture <name>
//! ```

#![feature(test)]

extern crate test;

use std::sync::atomic::{AtomicU64, Ordering};

use twizzler_queue_raw::{QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

/// Spin rather than sleep. On the host a syscall-backed wait is not representative of what the
/// real queue does, and the uncontended path never calls these at all.
fn wait(x: &AtomicU64, v: u64) {
    while x.load(Ordering::SeqCst) == v {
        core::hint::spin_loop();
    }
}

fn wake(_x: &AtomicU64) {}

/// The uncontended SPSC path: one producer, one consumer, queue never full or empty for long.
///
/// Host-side numbers are representative here precisely because this path makes no callbacks at all
/// (see `uncontended_path_makes_no_callbacks` in the unit tests) — there is no syscall for the host
/// to get wrong, so what is measured is the algorithm.
#[bench]
fn bench_spsc_round_trip(b: &mut test::Bencher) {
    let qh = RawQueueHdr::new(10, size_of::<QueueEntry<u64>>());
    let mut buffer = vec![QueueEntry::<u64>::default(); 1 << 10];
    let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

    let mut i = 0u64;
    b.iter(|| {
        q.submit(QueueEntry::new(0, i), wait, wake, SubmissionFlags::empty())
            .unwrap();
        let e = q.receive(wait, wake, ReceiveFlags::empty()).unwrap();
        i += 1;
        test::black_box(e.item())
    });
}

/// The same path pipelined: fill a batch, then drain it. Closer to how a server queue is actually
/// driven, and it separates the per-item cost from the handoff.
#[bench]
fn bench_spsc_batched(b: &mut test::Bencher) {
    const BATCH: u64 = 64;

    let qh = RawQueueHdr::new(10, size_of::<QueueEntry<u64>>());
    let mut buffer = vec![QueueEntry::<u64>::default(); 1 << 10];
    let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

    b.iter(|| {
        for i in 0..BATCH {
            q.submit(QueueEntry::new(0, i), wait, wake, SubmissionFlags::empty())
                .unwrap();
        }
        for _ in 0..BATCH {
            test::black_box(q.receive(wait, wake, ReceiveFlags::empty()).unwrap().item());
        }
    });
}

/// The same path with the producer and consumer on separate threads, so `head`, `tail`, and `bell`
/// actually move between cores. `b.iter` times the producer's submit alone; the consumer runs
/// continuously underneath, so this is steady-state per-item cost, not a handoff.
///
/// Pin it to two distinct physical cores for a stable number — on this host, SMT siblings are `n`
/// and `n+16`, so e.g. `taskset -c 8,9`.
#[bench]
fn bench_spsc_cross_thread(b: &mut test::Bencher) {
    let qh = RawQueueHdr::new(10, size_of::<QueueEntry<u64>>());
    let mut buffer = vec![QueueEntry::<u64>::default(); 1 << 10];
    let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };
    let stop = AtomicU64::new(0);

    std::thread::scope(|s| {
        s.spawn(|| {
            while stop.load(Ordering::Relaxed) == 0 {
                if let Ok(e) = q.receive(wait, wake, ReceiveFlags::NON_BLOCK) {
                    test::black_box(e.item());
                }
            }
            while q.receive(wait, wake, ReceiveFlags::NON_BLOCK).is_ok() {}
        });

        let mut i = 0u64;
        b.iter(|| {
            q.submit(QueueEntry::new(0, i), wait, wake, SubmissionFlags::empty())
                .unwrap();
            i += 1;
        });
        stop.store(1, Ordering::Relaxed);
    });
}

/// Run `count` items through a fresh queue with the producer here and the consumer on another
/// thread, returning per-item one-way latencies in nanoseconds and the wall time for the whole run.
///
/// `pace` keeps at most one item in flight. Without it the producer outruns the consumer and every
/// latency is dominated by queueing delay (roughly queue depth times per-item cost), which measures
/// the backlog rather than the handoff; with it the queue stays shallow and what is left is the
/// transfer itself.
///
/// Latencies are a difference of two `Instant` reads taken on different threads off one epoch. The
/// producer samples before submitting and the consumer after receiving, so roughly one clock read's
/// worth of overhead falls inside each sample (the tail of one call plus the head of the other).
/// That is calibrated and reported alongside, because on some hosts it is the same order of
/// magnitude as the latency being measured.
fn run_two_thread(l2len: usize, count: usize, pace: bool) -> (Vec<u64>, std::time::Duration) {
    use std::time::Instant;

    let qh = RawQueueHdr::new(l2len, size_of::<QueueEntry<u64>>());
    let mut buffer = vec![QueueEntry::<u64>::default(); 1 << l2len];
    let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

    let epoch = Instant::now();
    let received = AtomicU64::new(0);

    std::thread::scope(|s| {
        let consumer = s.spawn(|| {
            let mut lats = Vec::with_capacity(count);
            for _ in 0..count {
                let e = q.receive(wait, wake, ReceiveFlags::empty()).unwrap();
                let now = epoch.elapsed().as_nanos() as u64;
                lats.push(now.saturating_sub(e.item()));
                // Only the paced run needs this; unconditionally bumping a shared counter would
                // put cross-core traffic on the throughput measurement.
                if pace {
                    received.fetch_add(1, Ordering::Release);
                }
            }
            lats
        });

        let start = Instant::now();
        for i in 0..count {
            let stamp = epoch.elapsed().as_nanos() as u64;
            q.submit(
                QueueEntry::new(i as u32, stamp),
                wait,
                wake,
                SubmissionFlags::empty(),
            )
            .unwrap();
            if pace {
                while received.load(Ordering::Acquire) <= i as u64 {
                    core::hint::spin_loop();
                }
            }
        }
        let lats = consumer.join().unwrap();
        (lats, start.elapsed())
    })
}

/// Throughput and per-item latency for one producer and one consumer on separate threads.
///
/// Reports rather than asserts a target: the numbers are machine-specific, and a threshold that
/// holds on the build host would be noise on anything else.
#[test]
fn spsc_two_thread_report() {
    use std::time::Instant;

    const SATURATED: usize = 200_000;
    const PACED: usize = 20_000;

    fn pct(sorted: &[u64], p: f64) -> u64 {
        sorted[(((sorted.len() - 1) as f64) * p) as usize]
    }

    fn report(name: &str, mut lats: Vec<u64>, elapsed: std::time::Duration) {
        let n = lats.len();
        let mean = lats.iter().sum::<u64>() as f64 / n as f64;
        lats.sort_unstable();
        println!(
            "{name}: {n} items in {:.3} ms -> {:.2} M items/s; latency mean {:.0} ns, \
             p50 {} ns, p99 {} ns, max {} ns",
            elapsed.as_secs_f64() * 1e3,
            n as f64 / elapsed.as_secs_f64() / 1e6,
            mean,
            pct(&lats, 0.50),
            pct(&lats, 0.99),
            lats[n - 1],
        );
    }

    // Calibrate the clock reads folded into every latency below. This also doubles as a read on how
    // disturbed the run was: it varies about 2x with host load.
    let epoch = Instant::now();
    let start = Instant::now();
    for _ in 0..10_000 {
        test::black_box(epoch.elapsed().as_nanos() as u64);
    }
    println!(
        "clock read: {:.1} ns (about one of these is folded into each latency below)",
        start.elapsed().as_nanos() as f64 / 10_000.0
    );

    let (lats, elapsed) = run_two_thread(10, SATURATED, false);
    assert_eq!(lats.len(), SATURATED);
    report("saturated", lats, elapsed);

    let (lats, elapsed) = run_two_thread(10, PACED, true);
    assert_eq!(lats.len(), PACED);
    report("paced (one in flight)", lats, elapsed);
}

/// `n` queues laid out either the way `Queue::init` used to (each header alone in its own 4 KiB
/// region, four pages per duplex queue) or packed tight (one page for a small queue), driven
/// round-robin by one thread.
///
/// Buffers are deliberately tiny (16 slots) so the header pages dominate the footprint, and the
/// work is single-threaded so nothing measured here is coherence traffic. This is the evidence for
/// the packed layout `twizzler-queue`'s `Queue::init` now uses.
fn multi_queue_round_robin(n: usize, packed: bool, iters: usize) -> f64 {
    use std::time::Instant;

    #[repr(C, align(4096))]
    struct Page([u8; 4096]);

    const L2LEN: usize = 4;
    const SLOTS: usize = 1 << L2LEN;
    let esz = size_of::<QueueEntry<u64>>();
    let hsz = size_of::<RawQueueHdr>();

    let stride_pages = if packed { 1 } else { 4 };
    let mut mem: Vec<Page> = (0..n * stride_pages).map(|_| Page([0u8; 4096])).collect();
    let basep = mem.as_mut_ptr() as *mut u8;

    let mut queues = Vec::with_capacity(n);
    for q in 0..n {
        // Offsets of (sub_hdr, com_hdr, sub_buf, com_buf) within this queue's span. `hsz` is a
        // multiple of the header's 64-byte alignment, so the packed form stays aligned.
        let off = if packed {
            (0, hsz, 2 * hsz, 2 * hsz + SLOTS * esz)
        } else {
            (4096, 8192, 12288, 12288 + SLOTS * esz)
        };
        unsafe {
            let qbase = basep.add(q * stride_pages * 4096);
            let sh = qbase.add(off.0) as *mut RawQueueHdr;
            let ch = qbase.add(off.1) as *mut RawQueueHdr;
            sh.write(RawQueueHdr::new(L2LEN, esz));
            ch.write(RawQueueHdr::new(L2LEN, esz));
            queues.push((
                RawQueue::<u64>::new(sh, qbase.add(off.2) as *mut QueueEntry<u64>),
                RawQueue::<u64>::new(ch, qbase.add(off.3) as *mut QueueEntry<u64>),
            ));
        }
    }

    let start = Instant::now();
    for _ in 0..iters {
        for (sub, com) in &queues {
            sub.submit(QueueEntry::new(0, 1), wait, wake, SubmissionFlags::empty())
                .unwrap();
            let e = sub.receive(wait, wake, ReceiveFlags::empty()).unwrap();
            com.submit(
                QueueEntry::new(0, e.item()),
                wait,
                wake,
                SubmissionFlags::empty(),
            )
            .unwrap();
            test::black_box(
                com.receive(wait, wake, ReceiveFlags::empty())
                    .unwrap()
                    .item(),
            );
        }
    }
    start.elapsed().as_nanos() as f64 / (iters * n * 2) as f64
}

/// Does spreading a queue's headers across pages cost anything?
#[test]
fn page_layout_report() {
    println!(
        "header+buffer layout, single thread, 16-slot queues ({} B header)",
        size_of::<RawQueueHdr>()
    );
    for n in [1usize, 8, 32, 128, 512] {
        let iters = (200_000 / n).max(50);
        // Each config is freshly allocated, so the first pass pays page faults and a cold cache.
        // Discard one, then interleave the two layouts and keep the best of each: the minimum is
        // the run least disturbed by the rest of the machine.
        multi_queue_round_robin(n, false, iters);
        multi_queue_round_robin(n, true, iters);
        let (mut a, mut b) = (f64::MAX, f64::MAX);
        for _ in 0..5 {
            a = a.min(multi_queue_round_robin(n, false, iters));
            b = b.min(multi_queue_round_robin(n, true, iters));
        }
        println!(
            "  {n:4} queues: 4 pages/queue {a:6.1} ns/item, 1 page/queue {b:6.1} ns/item  \
             ({:+.1}%)",
            (b - a) / a * 100.0
        );
    }
}
