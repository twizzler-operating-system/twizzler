//! Loom models of the queue header's wake protocol.
//!
//! Run with `RUSTFLAGS="--cfg loom" cargo test -p twizzler-queue-raw`. Host-only and not wired into
//! xtask.
//!
//! The models split in two. The first three drive `RawQueueHdr` directly — the four counters and
//! the two waiting indicators, where every known wake-protocol bug has lived. The last two run
//! whole `RawQueue::{submit, receive}` calls over a real entry buffer, which additionally covers
//! the head CAS under multi-producer contention and the turn handshake.
//!
//! In the latter, `cmd_slot` stays on real atomics (see the atomics import in lib.rs — loom cannot
//! see an atomic punned out of a raw pointer into shared memory). Its accesses therefore execute
//! but are not permuted: loom explores the header interleavings around a turn handshake it treats
//! as opaque. That is a real gap, not a formality — it means these models can confirm
//! submit/receive agree under contention, but cannot rule out a reordering bug in the turn protocol
//! itself.
//!
//! [`Park`] stands in for `sys_thread_sync`, and models the part that matters: the sleeper's
//! predicate is evaluated once, atomically, at sleep time, and after that only an explicit wake
//! releases it. A value change alone does not — which is exactly why a lost `ring` is a hang rather
//! than a slow path. The shim's own handoff cannot drop a wake (its ringer takes the lock to notify
//! and its waiter holds that lock across the predicate check), so a deadlock reported by loom means
//! the queue failed to ring.
//!
//! These models depend on [`crate::sc_fence`] to work at all; see its comment.

use core::cell::UnsafeCell;

use loom::sync::{
    atomic::{AtomicU64, AtomicUsize, Ordering},
    Arc, Condvar, Mutex,
};

use crate::{QueueEntry, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

/// A one-slot queue with its single slot already reserved, i.e. full.
fn full_hdr() -> RawQueueHdr {
    let hdr = RawQueueHdr::new(0, 8);
    hdr.reserve_slot(SubmissionFlags::NON_BLOCK, |_, _| {})
        .unwrap();
    hdr
}

/// An async submitter that arms a sleep on `tail` must be woken by the consumer that drains.
///
/// Covers the sticky `ASYNC_SUBMIT_WAITING` bit: the submitter has nowhere to run a matching
/// decrement, so `advance_tail` consumes the bit before its store and re-checks after it.
#[test]
fn async_submitter_wakeup_is_not_lost() {
    loom::model(|| {
        let hdr = Arc::new(full_hdr());
        let park = Arc::new(Park::new());

        let consumer = {
            let (hdr, park) = (hdr.clone(), park.clone());
            loom::thread::spawn(move || hdr.advance_tail(|w| park.ring(w)))
        };

        let (word, value) = hdr.setup_send_sleep_simple();
        park.wait(word, value);

        consumer.join().unwrap();
    });
}

/// A submitter blocking inside `reserve_slot` on a full queue must make progress once drained.
///
/// The counted (`inc`/`dec_submit_waiting`) path, as opposed to the sticky bit above.
#[test]
fn blocking_submitter_makes_progress() {
    loom::model(|| {
        let hdr = Arc::new(full_hdr());
        let park = Arc::new(Park::new());

        let consumer = {
            let (hdr, park) = (hdr.clone(), park.clone());
            loom::thread::spawn(move || hdr.advance_tail(|w| park.ring(w)))
        };

        hdr.reserve_slot(SubmissionFlags::empty(), |w, v| park.wait(w, v))
            .unwrap();

        consumer.join().unwrap();
    });
}

/// A consumer that arms a sleep on `bell` must still be woken by a producer publishing, even when a
/// second consumer thread drains concurrently.
///
/// The split between the poller arming and the executor draining is the real async arrangement, not
/// a contrived one. This deadlocked before `advance_tail` stopped clearing the flag: its old
/// load/store pair both masked the bit off unconditionally and dropped any `fetch_or` landing
/// between its halves, so the producer's `ring` saw no waiter and skipped the wake.
#[test]
fn consumer_wakeup_survives_concurrent_advance_tail() {
    loom::model(|| {
        let hdr = Arc::new(RawQueueHdr::new(1, 8));
        // An earlier submission, which the executor is about to finish receiving.
        hdr.ring(|_| {});
        let park = Arc::new(Park::new());

        // Arm before spawning: a producer that publishes *after* the consumer arms owes it a wake,
        // which makes a missed wake here unambiguous. Arming concurrently would also admit the
        // benign end state where the consumer is simply waiting on an empty queue.
        let (word, value) = hdr.setup_rec_sleep_simple();

        let drainer = {
            let (hdr, park) = (hdr.clone(), park.clone());
            loom::thread::spawn(move || hdr.advance_tail(|w| park.ring(w)))
        };
        let producer = {
            let (hdr, park) = (hdr.clone(), park.clone());
            loom::thread::spawn(move || hdr.ring(|w| park.ring(w)))
        };

        park.wait(word, value);

        drainer.join().unwrap();
        producer.join().unwrap();
    });
}

/// Once a wake has been delivered, later submissions must not keep paying for wakes nobody is
/// waiting for.
///
/// The safety direction — never miss a wake — is covered above. This is the efficiency direction,
/// and it is the one that regressed when `advance_tail` stopped clearing the flag as a side effect
/// of masking: without an explicit teardown, every submission after the first async arm rang.
#[test]
fn honoured_wake_consumes_the_arm() {
    loom::model(|| {
        let hdr = Arc::new(RawQueueHdr::new(1, 8));
        let park = Arc::new(Park::new());

        let (word, value) = hdr.setup_rec_sleep_simple();

        let producer = {
            let (hdr, park) = (hdr.clone(), park.clone());
            loom::thread::spawn(move || {
                hdr.ring(|w| park.ring(w));
                hdr.ring(|w| park.ring(w));
            })
        };

        park.wait(word, value);
        producer.join().unwrap();

        // The consumer armed once, so at most one of the two submissions owes it a wake.
        let (_, rings) = park.counts();
        assert!(rings <= 1, "rings={rings}");
    });
}

/// A header plus its entry buffer, so a model can run whole `submit`/`receive` calls.
struct SharedQueue {
    hdr: RawQueueHdr,
    buf: UnsafeCell<[QueueEntry<u32>; SLOTS]>,
}

const SLOTS: usize = 2;

unsafe impl Sync for SharedQueue {}
unsafe impl Send for SharedQueue {}

impl SharedQueue {
    fn new() -> Self {
        Self {
            hdr: RawQueueHdr::new(
                SLOTS.ilog2() as usize,
                core::mem::size_of::<QueueEntry<u32>>(),
            ),
            buf: UnsafeCell::new([QueueEntry::default(); SLOTS]),
        }
    }

    fn queue(&self) -> RawQueue<u32> {
        unsafe { RawQueue::new(&self.hdr, self.buf.get().cast()) }
    }
}

/// A producer and a consumer running concurrently must hand the item over intact, with the consumer
/// blocking on `bell` until the producer publishes.
///
/// The consumer-side wake path end to end: `get_next_ready` arms `consumer_set_waiting`, `ring`
/// observes it and wakes.
#[test]
fn submit_receive_round_trip() {
    loom::model(|| {
        let q = Arc::new(SharedQueue::new());
        let park = Arc::new(Park::new());

        let producer = {
            let (q, park) = (q.clone(), park.clone());
            loom::thread::spawn(move || {
                q.queue()
                    .submit(
                        QueueEntry::new(7, 42),
                        |w, v| park.wait(w, v),
                        |w| park.ring(w),
                        SubmissionFlags::empty(),
                    )
                    .unwrap()
            })
        };

        let item = q
            .queue()
            .receive(
                |w, v| park.wait(w, v),
                |w| park.ring(w),
                ReceiveFlags::empty(),
            )
            .unwrap();
        assert_eq!(item.info(), 7);
        assert_eq!(item.item(), 42);

        producer.join().unwrap();

        // The producer never blocks (there is room) and the consumer blocks at most once, so one
        // sleep and its matching wake is the ceiling for handing over a single item.
        let (waits, rings) = park.counts();
        assert!(waits <= 1 && rings <= 1, "waits={waits} rings={rings}");
    });
}

/// Concurrent producers must get distinct slots, and both items must survive.
///
/// This is the multi-producer half of MPSC: the `head` compare-exchange and its retry path are the
/// only thing keeping two submitters off the same slot.
#[test]
fn concurrent_producers_get_distinct_slots() {
    loom::model(|| {
        let q = Arc::new(SharedQueue::new());
        let park = Arc::new(Park::new());

        let other = {
            let (q, park) = (q.clone(), park.clone());
            loom::thread::spawn(move || {
                q.queue()
                    .submit(
                        QueueEntry::new(2, 20),
                        |w, v| park.wait(w, v),
                        |w| park.ring(w),
                        SubmissionFlags::empty(),
                    )
                    .unwrap()
            })
        };

        q.queue()
            .submit(
                QueueEntry::new(1, 10),
                |w, v| park.wait(w, v),
                |w| park.ring(w),
                SubmissionFlags::empty(),
            )
            .unwrap();
        other.join().unwrap();

        // Both are published by now, so neither receive can block.
        let queue = q.queue();
        let mut got = [
            queue
                .receive(|_, _| {}, |_| {}, ReceiveFlags::NON_BLOCK)
                .unwrap()
                .item(),
            queue
                .receive(|_, _| {}, |_| {}, ReceiveFlags::NON_BLOCK)
                .unwrap()
                .item(),
        ];
        got.sort();
        assert_eq!(got, [10, 20]);

        // Nothing was ever full and no consumer had armed, so contention between producers must
        // stay entirely in userspace.
        assert_eq!(park.counts(), (0, 0));
    });
}

struct Park {
    lock: Mutex<()>,
    cv: Condvar,
    waits: AtomicUsize,
    rings: AtomicUsize,
}

impl Park {
    fn new() -> Self {
        Self {
            lock: Mutex::new(()),
            cv: Condvar::new(),
            waits: AtomicUsize::new(0),
            rings: AtomicUsize::new(0),
        }
    }

    /// Each callback invocation is one `sys_thread_sync` in the real queue, so the models bound
    /// them rather than only checking that they happen.
    fn counts(&self) -> (usize, usize) {
        (
            self.waits.load(Ordering::SeqCst),
            self.rings.load(Ordering::SeqCst),
        )
    }

    fn wait(&self, word: &AtomicU64, value: u64) {
        self.waits.fetch_add(1, Ordering::SeqCst);
        let mut guard = self.lock.lock().unwrap();
        while word.load(Ordering::SeqCst) == value {
            guard = self.cv.wait(guard).unwrap();
        }
    }

    fn ring(&self, _word: &AtomicU64) {
        self.rings.fetch_add(1, Ordering::SeqCst);
        let _guard = self.lock.lock().unwrap();
        self.cv.notify_all();
    }
}
