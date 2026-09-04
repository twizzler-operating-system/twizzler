//! A raw queue interface for Twizzler, making no assumptions about where the underlying headers and
//! circular buffers are located. This means you probably don't want to use this --- instead, I
//! suggest you use the wrapped version of this library, twizzler-queue, since that actually
//! interacts with the object system.
//!
//! This library exists to provide an underlying implementation of the concurrent data structure for
//! each individual raw queue so that this complex code can be reused in both userspace and the
//! kernel.
//!
//! The basic design of a raw queue is two parts:
//!
//!   1. A header, which contains things like head pointers, tail pointers, etc.
//!   2. A buffer, which contains the items that are enqueued.
//!
//! The queue is an MPSC lock-free blocking data structure. Any thread may submit to a queue, but
//! only one thread may receive on that queue at a time. The queue is implemented with a head
//! pointer, a tail pointer, a doorbell, and a waiters counter. Additionally, the queue is
//! maintained in terms of "turns", that indicate which "go around" of the queue we are on (mod 2).
//!
//! # Let's look at an insert
//! Here's what the queue looks like to start with. The 0_ indicates that it's empty, and turn is
//! set to 0.
//! ```text
//!  b
//!  t
//!  h
//! [0_, 0_, 0_]
//! ```
//! When inserting, the thread first reserves space:
//! ```text
//!  b
//!  t
//!      h
//! [0_, 0_, 0_]
//! ```
//! Then it fills out the data:
//! ```text
//!  b
//!  t
//!      h
//! [0X, 0_, 0_]
//! ```
//! Then it toggles the turn bit:
//! ```text
//!  b
//!  t
//!      h
//! [1X, 0_, 0_]
//! ```
//! Next, it bumps the doorbell (and maybe wakes up a waiting consumer):
//! ```text
//!      b
//!  t
//!      h
//! [1X, 0_, 0_]
//! ```
//!
//! Now, let's say the consumer comes along and dequeues. First, it checks if it's empty by
//! comparing tail and bell, and finds it's not empty. Then it checks if it's the correct turn. This
//! turn is 1, so yes. Next, it remove the data from the queue:
//! ```text
//!      b
//!  t
//!      h
//! [1_, 0_, 0_]
//! ```
//! And then finally it increments the tail counter:
//! ```text
//!      b
//!      t
//!      h
//! [1_, 0_, 0_]
//! ```

#![cfg_attr(not(any(feature = "std", test)), no_std)]

#[cfg(not(loom))]
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use core::{cell::UnsafeCell, fmt::Display, marker::PhantomData, ptr::addr_of_mut};

// Under `--cfg loom` the header's counters become loom-tracked atomics so its wake protocol
// can be model-checked. The entry buffer stays on real atomics either way: loom cannot see an
// atomic punned out of a raw pointer into shared memory, so the buffer is deliberately outside
// the model.
#[cfg(loom)]
use loom::sync::atomic::{AtomicU32, AtomicU64, Ordering};

#[cfg(not(loom))]
pub const SPIN_ATTEMPTS: usize = 1000;
// Loom treats every spin iteration as a scheduling point; 1000 of them is an unexplorable state
// space, and the interesting transitions are all past the spin phase anyway.
#[cfg(loom)]
pub const SPIN_ATTEMPTS: usize = 1;

/// Compensates for loom modelling `SeqCst` as AcqRel, restoring the total order only at an explicit
/// fence. Each call site below sits on a store-buffer boundary — one side stores its waiting
/// indicator then loads the counter, the other stores the counter then loads the indicator — whose
/// correctness *is* the SC guarantee that at least one side observes the other. Without this,
/// models of the wake protocol report lost wakeups that C11 SC (and both targets' codegen) forbid.
/// Compiles to nothing outside `--cfg loom`.
#[inline(always)]
fn sc_fence() {
    #[cfg(loom)]
    loom::sync::atomic::fence(Ordering::SeqCst);
}

#[inline]
fn spin_hint() {
    #[cfg(loom)]
    loom::thread::yield_now();
    #[cfg(not(loom))]
    core::hint::spin_loop();
}

#[cfg(loom)]
mod loom_tests;

#[derive(Clone, Copy, Default, Debug)]
#[repr(C)]
/// A queue entry. All queues must be formed of these, as the queue algorithm uses data inside this
/// struct as part of its operation. The cmd_slot is used internally to track turn, and the info is
/// used by the full queue structure to manage completion. The data T is user data passed around the
/// queue.
pub struct QueueEntry<T> {
    cmd_slot: u32,
    info: u32,
    data: T,
}

impl<T> QueueEntry<T> {
    /// Atomic access to `cmd_slot`, derived from a mutable-provenance pointer to the entry rather
    /// than a shared reference: the sibling fields are written non-atomically by `submit` and read
    /// non-atomically by `receive`, and only the turn protocol keeps those from overlapping with
    /// this word.
    ///
    /// # Safety
    /// `item` must point to a live, aligned entry in the queue buffer.
    #[inline]
    unsafe fn get_cmd_slot(item: *mut Self) -> u32 {
        unsafe {
            core::sync::atomic::AtomicU32::from_ptr(addr_of_mut!((*item).cmd_slot))
                .load(core::sync::atomic::Ordering::SeqCst)
        }
    }

    /// # Safety
    /// As [QueueEntry::get_cmd_slot].
    #[inline]
    unsafe fn set_cmd_slot(item: *mut Self, v: u32) {
        unsafe {
            core::sync::atomic::AtomicU32::from_ptr(addr_of_mut!((*item).cmd_slot))
                .store(v, core::sync::atomic::Ordering::SeqCst)
        }
    }

    #[inline]
    /// Get the data item of a QueueEntry.
    pub fn item(self) -> T {
        self.data
    }

    #[inline]
    /// Get the info tag of a QueueEntry.
    pub fn info(&self) -> u32 {
        self.info
    }

    /// Construct a new QueueEntry. The `info` tag should be used to inform completion events in the
    /// full queue.
    pub fn new(info: u32, item: T) -> Self {
        Self {
            cmd_slot: 0,
            info,
            data: item,
        }
    }
}

/// The base info structure stored in a Twizzler queue object. Used to open Twizzler queue objects
/// and create a Queue.
#[repr(C)]
pub struct QueueBase<S, C> {
    pub sub_hdr: usize,
    pub com_hdr: usize,
    pub sub_buf: usize,
    pub com_buf: usize,
    _pd: PhantomData<(S, C)>,
}

/// Top bit of `waiters`: an async submitter has armed a sleep on `tail`. The low bits remain the
/// blocking submitters' count.
const ASYNC_SUBMIT_WAITING: u32 = 1 << 31;

/// `consumer_waiting`: the consumer has armed a sleep on `bell`.
///
/// This lived in bit 63 of `tail` for a while, deliberately outside that counter's arithmetic
/// range so `advance_tail` could stay a plain `fetch_add(1)` that could not clobber it. Its own
/// word keeps that property and drops the reason it was ever packed: the producer reads this flag
/// on *every* submission, and `tail` is written by the consumer on every receive, so sharing a line
/// with the counter made that read a guaranteed coherence miss. Alone on a line it is written only
/// when someone actually arms a sleep, so it stays clean-shared in the producer's cache.
///
/// It also removes the one wart of the packed form: `setup_send_sleep_simple` returns `tail`
/// unmasked, so a flag inside that word could flip the sleep predicate and cost a spurious wake.
const CONSUMER_WAITING: u32 = 1;

/// Pads a field out to its own cache line.
///
/// The whole header used to fit inside one 64-byte line: producer-written `head` and `bell`,
/// consumer-written `tail`, and the read-only geometry all together. Every access from either side
/// invalidated the other's copy of the line, so on the cross-core path essentially every atomic in
/// the algorithm was a coherence miss — which is most of the gap between the single-threaded and
/// two-thread numbers.
///
/// 64 bytes matches both targets we build for. Over-padding costs address space in a region that
/// has 4 KiB to spare (see `Queue::init`'s `HDR_LEN`); under-padding silently reintroduces the
/// sharing, so this does not try to be clever about it.
#[repr(C, align(64))]
struct CacheLine<T>(T);

impl<T> core::ops::Deref for CacheLine<T> {
    type Target = T;
    #[inline]
    fn deref(&self) -> &T {
        &self.0
    }
}

/// The producer-owned line: `head`, plus the producers' cached view of `tail`.
///
/// Both are written only by submitters, so they belong on one line — the cache is free to consult
/// once `head` is in hand, and putting it anywhere else would just add a line to the working set.
#[repr(C)]
struct ProducerState {
    head: AtomicU32,
    /// Last value of `tail` a producer observed, masked to 31 bits like every other reader of it.
    ///
    /// `is_full` is the only thing on the submission fast path that needs `tail`, and `tail` is
    /// the word the consumer writes on every receive — so consulting it directly made every
    /// submission pay a coherence miss just to be told, almost always, that the queue has
    /// room.
    ///
    /// Safe to trust because `tail` only ever advances: a stale copy *understates* what the
    /// consumer has drained, so it can only report a queue as fuller than it is. That direction
    /// costs a refresh, never a slot handed out over live data. It also cannot go stale without
    /// bound — each submission advances `head`, so after at most `len` of them the cached value
    /// reports full and forces a real read.
    cached_tail: AtomicU32,
}

#[repr(C, align(64))]
/// A raw queue header. This contains all the necessary counters and info to run the queue
/// algorithm.
///
/// Field order is by writer, not by meaning: the geometry is immutable after init, `head` is
/// producer-written per submission, `waiters` is producer-written only when a submitter blocks,
/// `bell` is producer-written per submission, and `tail` is consumer-written per receive. The two
/// rarely-written words stay clean-shared in the readers' caches precisely because they do not
/// share a line with a counter that moves every item.
pub struct RawQueueHdr {
    l2len: usize,
    stride: usize,
    producer: CacheLine<ProducerState>,
    waiters: CacheLine<AtomicU32>,
    consumer_waiting: CacheLine<AtomicU32>,
    bell: CacheLine<AtomicU64>,
    tail: CacheLine<AtomicU64>,
}

/// Rings that found a consumer armed (a wake was actually sent).
pub static RING_WOKE: AtomicU64 = AtomicU64::new(0);
/// Rings that found no consumer armed (the entry was queued, nobody woken).
pub static RING_NO_WAITER: AtomicU64 = AtomicU64::new(0);

impl RawQueueHdr {
    /// Construct a new raw queue header.
    pub fn new(l2len: usize, stride: usize) -> Self {
        Self {
            l2len,
            stride,
            producer: CacheLine(ProducerState {
                head: AtomicU32::new(0),
                cached_tail: AtomicU32::new(0),
            }),
            waiters: CacheLine(AtomicU32::new(0)),
            consumer_waiting: CacheLine(AtomicU32::new(0)),
            bell: CacheLine(AtomicU64::new(0)),
            tail: CacheLine(AtomicU64::new(0)),
        }
    }

    pub fn len_bytes(&self) -> usize {
        self.len() * self.stride
    }

    #[inline]
    pub fn len(&self) -> usize {
        1 << self.l2len
    }

    #[inline]
    fn is_full(&self, h: u32, t: u64) -> bool {
        // `h` and `t` are separate loads, so `t` can legitimately be *ahead* of `h`: another
        // producer advanced head after we read it, and the consumer drained past our stale value.
        // Both counters also wrap in this 31-bit space -- tail is masked on store, head on every
        // comparison -- so the difference is modular, not arithmetic.
        //
        // Subtracting directly underflows. In an overflow-checked build that panics, which killed a
        // pager-srv worker mid-sweep and wedged the guest; in a release build it wraps to ~2^64,
        // which is `>= len`, so a queue with space reports itself permanently full and the producer
        // blocks forever. The release behaviour is the worse of the two.
        let outstanding = (h & 0x7fffffff).wrapping_sub((t & 0x7fffffff) as u32) & 0x7fffffff;
        // At or past the half-space, `t` is ahead of a stale `h` rather than a genuine backlog of
        // 2^30 items, so there is certainly room.
        outstanding < (1 << 30) && outstanding >= self.len() as u32
    }

    #[inline]
    fn is_empty(&self, bell: u64, tail: u64) -> bool {
        (bell & 0x7fffffff) == (tail & 0x7fffffff)
    }

    #[inline]
    fn is_turn<T>(&self, t: u64, item: *mut QueueEntry<T>) -> bool {
        let turn = (t / (self.len() as u64)) % 2;
        let val = unsafe { QueueEntry::get_cmd_slot(item) } >> 31;
        (val == 1) == (turn == 0)
    }

    #[inline]
    fn submitter_waiting(&self) -> bool {
        self.waiters.load(Ordering::SeqCst) > 0
    }

    /// A plain store, not a read-modify-write: the word holds nothing but this flag now, and both
    /// directions are idempotent. Note this does *not* make arming single-writer — a blocking
    /// receiver and a reactor thread can arm concurrently, since `with_guard` serializes receives
    /// and not arming — but concurrent arms agree on the value, and the clear could already race
    /// with an arm when the flag lived in `tail`. Unchanged semantics, one fewer locked RMW.
    #[inline]
    fn consumer_set_waiting(&self, waiting: bool) {
        self.consumer_waiting
            .store(if waiting { CONSUMER_WAITING } else { 0 }, Ordering::SeqCst);
    }

    /// Consume the consumer's armed flag, reporting whether it was set.
    ///
    /// The mirror of [RawQueueHdr::take_submitter_waiting], and for the same reason: a consumer
    /// re-arms before every sleep, so a wake that honours an arm should also retire it. Without
    /// this nothing tears the flag down at all -- `advance_tail` used to, accidentally, by masking
    /// -- and every submission after the first async arm pays a wake syscall for a consumer that is
    /// awake.
    ///
    /// Safe against a consumer arming concurrently: this side bumps `bell` before reading the flag
    /// and the consumer sets the flag before reading `bell`, so by the SeqCst total order a
    /// consumer whose arm is cleared here has already observed the new `bell` and will not sleep.
    /// The load guard matters: nobody is armed on the overwhelmingly common path, and an
    /// unconditional `fetch_and` would put a locked read-modify-write on every single submission.
    /// Skipping it when the flag is already clear is safe by the same argument as above — a
    /// consumer that arms after this load has yet to read `bell`, which this side has already
    /// bumped.
    #[inline]
    fn take_consumer_waiting(&self) -> bool {
        if self.consumer_waiting.load(Ordering::SeqCst) == 0 {
            return false;
        }
        self.consumer_waiting.swap(0, Ordering::SeqCst) != 0
    }

    #[inline]
    fn inc_submit_waiting(&self) {
        self.waiters.fetch_add(1, Ordering::SeqCst);
    }

    /// Register an async submitter that is about to arm a `ThreadSyncSleep` on `tail`.
    ///
    /// The blocking path brackets its wait with `inc`/`dec_submit_waiting`, but an async submitter
    /// arms a sleep and returns, so it has nowhere to run the `dec`. Hence a sticky bit rather than
    /// a count: `advance_tail` consumes it when it rings, which bounds the cost to one spurious
    /// ring. Using `inc_submit_waiting` here instead would leave `waiters` permanently non-zero
    /// after the first full queue, making every subsequent dequeue ring.
    #[inline]
    fn set_async_submit_waiting(&self) {
        self.waiters
            .fetch_or(ASYNC_SUBMIT_WAITING, Ordering::SeqCst);
    }

    #[inline]
    fn dec_submit_waiting(&self) {
        self.waiters.fetch_sub(1, Ordering::SeqCst);
    }

    /// Is there room for a submission at `h`, consulting the producers' cached `tail` first and
    /// only reading the real one if the cache claims the queue is full?
    ///
    /// This is the whole point of [ProducerState::cached_tail]: the answer is almost always yes,
    /// and getting it from the producer's own line rather than the consumer's turns the common
    /// submission into one that never touches consumer-written memory.
    ///
    /// One-directional by construction — the cache can only make the queue look fuller than it is
    /// (see the field's docs), so a `true` here is as trustworthy as one derived from a fresh load,
    /// while a `false` is merely a prompt to go and check for real.
    #[inline]
    fn has_room(&self, h: u32) -> bool {
        let cached = self.producer.cached_tail.load(Ordering::Relaxed) as u64;
        if !self.is_full(h, cached) {
            return true;
        }
        let t = self.tail.load(Ordering::SeqCst);
        self.producer
            .cached_tail
            .store((t & 0x7fffffff) as u32, Ordering::Relaxed);
        !self.is_full(h, t)
    }

    #[inline]
    fn reserve_slot<W: Fn(&AtomicU64, u64)>(
        &self,
        flags: SubmissionFlags,
        wait: W,
    ) -> Result<u32, QueueError> {
        let mut waiter = false;
        let mut attempts = SPIN_ATTEMPTS;
        let h = loop {
            let h = self.producer.head.load(Ordering::SeqCst);
            if self.has_room(h) {
                if self
                    .producer
                    .head
                    // Wrapping: head is only ever compared modulo 2^31, so the u32 rollover at 2^32
                    // submissions is benign -- but an overflow-checked build would panic on it.
                    .compare_exchange(h, h.wrapping_add(1), Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    break h;
                } else {
                    spin_hint();
                    continue;
                }
            }

            if flags.contains(SubmissionFlags::NON_BLOCK) {
                return Err(QueueError::WouldBlock);
            }

            if attempts != 0 {
                attempts -= 1;
                spin_hint();
                continue;
            }

            if !waiter {
                waiter = true;
                self.inc_submit_waiting();
                sc_fence();
            }

            let t = self.tail.load(Ordering::SeqCst);
            if self.is_full(h, t) {
                wait(&self.tail.0, t);
            }
        };

        if waiter {
            self.dec_submit_waiting();
        }

        Ok(h & 0x7fffffff)
    }

    #[inline]
    fn get_turn(&self, h: u32) -> bool {
        (h / self.len() as u32) % 2 == 0
    }

    #[inline]
    fn ring<R: Fn(&AtomicU64)>(&self, ring: R) {
        self.bell.fetch_add(1, Ordering::SeqCst);
        sc_fence();
        if self.take_consumer_waiting() {
            RING_WOKE.fetch_add(1, Ordering::Relaxed);
            ring(&self.bell.0)
        } else {
            // No consumer armed at the instant of the ring. A submission that lands here reaches
            // the queue but wakes nobody: if the consumer is between poll passes it will see the
            // entry on its next arm, and if it is parked on something else the entry waits. A high
            // ratio here against a dormant consumer is the difference between "the wake was lost"
            // and "there was no one to wake".
            RING_NO_WAITER.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn has_pending<T>(&self, raw_buf: *mut QueueEntry<T>) -> bool {
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        let b = self.bell.load(Ordering::SeqCst);
        let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };
        !self.is_empty(b, t) && self.is_turn(t, item)
    }

    /// `has_pending`'s two conjuncts, separately, plus the words they came from.
    ///
    /// `has_pending` answering false has two causes that no counter downstream can separate:
    /// nothing was ever submitted (`nonempty` false), or entries are present but their turn bit
    /// does not match the consumer's tail (`turn` false), which makes them invisible to `receive`
    /// as well and cannot be fixed by any wake. A single bool collapses those into one silence.
    pub fn pending_parts<T>(&self, raw_buf: *mut QueueEntry<T>) -> (u64, u64, bool, bool) {
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        let b = self.bell.load(Ordering::SeqCst);
        let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };
        (b, t, !self.is_empty(b, t), self.is_turn(t, item))
    }

    pub fn has_space<T>(&self) -> bool {
        let h = self.producer.head.load(Ordering::SeqCst);
        let t = self.tail.load(Ordering::SeqCst);
        !self.is_full(h, t)
    }

    #[inline]
    fn get_next_ready<W: Fn(&AtomicU64, u64), T>(
        &self,
        spin_attempts: usize,
        wait: W,
        flags: ReceiveFlags,
        raw_buf: *mut QueueEntry<T>,
    ) -> Result<(u64, usize), QueueError> {
        let mut attempts = spin_attempts;
        let t = loop {
            let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
            let b = self.bell.load(Ordering::SeqCst);
            let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };

            if !self.is_empty(b, t) && self.is_turn(t, item) {
                break t;
            }

            if flags.contains(ReceiveFlags::NON_BLOCK) {
                return Err(QueueError::WouldBlock);
            }

            if attempts != 0 {
                attempts -= 1;
                spin_hint();
                continue;
            }

            self.consumer_set_waiting(true);
            sc_fence();
            let b = self.bell.load(Ordering::SeqCst);
            if self.is_empty(b, t) || !self.is_turn(t, item) {
                wait(&self.bell.0, b);
            }
        };

        if attempts == 0 {
            self.consumer_set_waiting(false);
        }
        Ok((t, spin_attempts - attempts))
    }

    /// Arm a sleep on `bell` for a caller that will not block inside the queue.
    ///
    /// There is deliberately no paired cancel. `consumer_waiting` is a single bit with two arming
    /// paths, and the single-consumer rule does not separate them: it serializes *receives*, so
    /// `get_next_ready`'s blocking loop arms from inside one, while this path arms from a reactor
    /// thread that is not receiving at all and so is not covered by that guard. A cancel here could
    /// therefore clear an arm a blocking receiver is asleep on, reintroducing the missed wake that
    /// moving the flag to [CONSUMER_WAITING] fixed. `QueueSender` reaches both paths on a single
    /// completion subqueue today, so this is a live configuration rather than a hypothetical one.
    ///
    /// An abandoned arm is retired instead by the first producer to honour it
    /// (`take_consumer_waiting`), bounding it to one stray wake rather than one per submission.
    fn setup_rec_sleep_simple(&self) -> (&AtomicU64, u64) {
        // Load BEFORE arming, not after. The old order (arm, fence, load) had a one-ring hole:
        // a producer ringing between the arm and the load consumes the arm (its wake lands on
        // nobody -- the consumer has not parked) AND its bump is included in the captured
        // value, so the kernel accepts the park and the consumer then sleeps with
        // consumer_waiting clear -- every later ring takes the RING_NO_WAITER arm and the
        // sleeper starves while the bell climbs (hang rows: SYNC/DONE intact, cw=0, wv
        // drifting past av). Capturing first closes it: any ring that could consume the fresh
        // arm bumps the bell past the captured value, and the kernel refuses the park -- a
        // spurious return, which every caller loops on.
        //
        // The sleep value has to come from bell, the word being slept on -- not from tail. bell
        // free-runs while tail is masked to 31 bits, so the two agree only for the first 2^31
        // operations; past that a tail-derived value never matches bell, the sleep returns
        // immediately every time, and the caller spins instead of blocking.
        let b = self.bell.load(Ordering::SeqCst);
        self.consumer_set_waiting(true);
        sc_fence();
        (&self.bell.0, b)
    }

    fn setup_send_sleep_simple(&self) -> (&AtomicU64, u64) {
        // Must be set before the sleep value is read below: a consumer that advances tail after
        // this point has to see a waiter and ring, or the sleep is never woken.
        self.set_async_submit_waiting();
        sc_fence();
        // Unmasked: the sleep predicate compares the whole 64-bit word, so masking off the
        // consumer-waiting bit makes the comparison fail on entry whenever that bit happens to be
        // set, turning the sleep into a busy poll. Leaving it in costs at most a spurious wake when
        // the bit flips.
        let t = self.tail.load(Ordering::SeqCst);
        let h = self.producer.head.load(Ordering::SeqCst);
        if self.is_full(h, t) {
            (&self.tail.0, t)
        } else {
            (&self.tail.0, u64::MAX)
        }
    }

    fn setup_rec_sleep<'a, T>(
        &'a self,
        sleep: bool,
        raw_buf: *mut QueueEntry<T>,
        waiter: &mut (Option<&'a AtomicU64>, u64),
    ) -> Result<u64, QueueError> {
        let t = self.tail.load(Ordering::SeqCst) & 0x7fffffff;
        let b = self.bell.load(Ordering::SeqCst);
        let item = unsafe { raw_buf.add((t as usize) & (self.len() - 1)) };
        *waiter = (Some(&self.bell.0), b);
        if self.is_empty(b, t) || !self.is_turn(t, item) {
            if sleep {
                self.consumer_set_waiting(true);
                sc_fence();
                // Keep the PRE-arm `b` as the sleep value; re-load only to detect readiness.
                // Sleeping on the post-arm value re-opens the one-ring hole closed in
                // setup_rec_sleep_simple: a ring between the arm and the re-load has consumed
                // the arm, and a sleep armed on the newer value parks a consumer whose flag is
                // already spent. With the pre-arm value, that ring makes the kernel refuse the
                // park instead (bell moved past it) -- a spurious return the caller loops on.
                let b2 = self.bell.load(Ordering::SeqCst);
                if !self.is_empty(b2, t) && self.is_turn(t, item) {
                    return Ok(t);
                }
            }
            Err(QueueError::WouldBlock)
        } else {
            Ok(t)
        }
    }

    /// Consume the async-submitter sticky bit, if set, *before* the tail store, and report whether
    /// anyone was waiting beforehand.
    ///
    /// Clearing before the store is what makes the handoff race-free: a submitter that arms after
    /// the clear re-sets the bit, and the post-store re-check in the callers picks it up. Clearing
    /// afterwards could wipe a bit set by a submitter that had already read the new tail, which is
    /// the lost wakeup this bit exists to prevent.
    ///
    /// That re-check is one half of a store-buffer pair — this side stores `tail` then loads
    /// `waiters`, the submitter side stores `waiters` then loads `tail` — so it relies on the
    /// SeqCst total order for at least one side to observe the other. Weakening any of the four
    /// accesses to Release/Acquire silently reintroduces the lost wakeup.
    #[inline]
    fn take_submitter_waiting(&self) -> bool {
        let w = self.waiters.load(Ordering::SeqCst);
        if w & ASYNC_SUBMIT_WAITING != 0 {
            self.waiters
                .fetch_and(!ASYNC_SUBMIT_WAITING, Ordering::SeqCst);
        }
        w != 0
    }

    /// Advance `tail` by one, and report whether a submitter needs waking.
    ///
    /// A single `fetch_add` rather than a load/store pair or a CAS loop: `tail` carries nothing but
    /// the counter — [CONSUMER_WAITING] has its own word — so nothing here has to preserve, mask,
    /// or re-read a flag, and the operation stays wait-free. The load/store form this replaced
    /// silently dropped any `consumer_set_waiting` landing between its two halves, and its mask
    /// cleared the flag even when it didn't — either way the next producer saw no waiter and
    /// skipped the wake.
    ///
    /// Consequence: nothing tears the consumer's flag down here, so it can outlive the last
    /// consumer that armed a sleep, costing an occasional wake syscall that wakes nobody.
    /// `get_next_ready` still clears it explicitly for the case it owns. A spurious wake is a
    /// wasted syscall; a missed one is a hang.
    #[inline]
    fn advance_tail<R: Fn(&AtomicU64)>(&self, ring: R) {
        let was_waiting = self.take_submitter_waiting();
        self.tail.fetch_add(1, Ordering::SeqCst);
        sc_fence();
        if was_waiting || self.submitter_waiting() {
            ring(&self.tail.0);
        }
    }

    #[inline]
    fn advance_tail_setup<'a>(&'a self, ringer: &mut Option<&'a AtomicU64>) {
        let was_waiting = self.take_submitter_waiting();
        self.tail.fetch_add(1, Ordering::SeqCst);
        sc_fence();
        if was_waiting || self.submitter_waiting() {
            *ringer = Some(&self.tail.0);
        }
    }
}

/// A raw queue, comprising of a header to track the algorithm and a buffer to hold queue entries.
pub struct RawQueue<T> {
    hdr: *const RawQueueHdr,
    buf: UnsafeCell<*mut QueueEntry<T>>,
}

bitflags::bitflags! {
    /// Flags to control how queue submission works.
    pub struct SubmissionFlags: u32 {
        /// If the request would block, return Err([SubmissionError::WouldBlock]) instead.
        const NON_BLOCK = 1;
    }

    /// Flags to control how queue receive works.
    pub struct ReceiveFlags: u32 {
        /// If the request would block, return Err([ReceiveError::WouldBlock]) instead.
        const NON_BLOCK = 1;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
/// Possible errors for submitting to a queue.
pub enum QueueError {
    /// An unknown error.
    Unknown,
    /// The operation would have blocked, and non-blocking operation was specified.
    WouldBlock,
}

impl Display for QueueError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unknown => write!(f, "unknown"),
            Self::WouldBlock => write!(f, "would block"),
        }
    }
}

impl core::error::Error for QueueError {}

#[cfg(feature = "std")]
impl From<QueueError> for std::io::Error {
    fn from(err: QueueError) -> Self {
        match err {
            QueueError::WouldBlock => std::io::Error::from(std::io::ErrorKind::WouldBlock),
            _ => std::io::Error::from(std::io::ErrorKind::Other),
        }
    }
}

impl<T: Copy> RawQueue<T> {
    /// Construct a new raw queue out of a header reference and a buffer pointer.
    /// # Safety
    /// The caller must ensure that hdr and buf point to valid objects, and that the lifetime of the
    /// RawQueue is exceeded by the objects pointed to.
    pub unsafe fn new(hdr: *const RawQueueHdr, buf: *mut QueueEntry<T>) -> Self {
        Self {
            hdr,
            buf: UnsafeCell::new(buf),
        }
    }

    #[inline]
    pub fn hdr(&self) -> &RawQueueHdr {
        unsafe { &*self.hdr }
    }

    #[inline]
    fn get_buf(&self, off: usize) -> *mut QueueEntry<T> {
        unsafe { (*self.buf.get()).add(off & (self.hdr().len() - 1)) }
    }

    /// Submit a data item of type T, wrapped in a QueueEntry, to the queue. The two callbacks,
    /// wait, and ring, are for implementing a rudimentary condvar, wherein if the queue needs to
    /// block, we'll call wait(x, y), where we are supposed to wait until *x != y. Once we are done
    /// inserting, if we need to wake up a consumer, we will call ring, which should wake up anyone
    /// waiting on that word of memory.
    pub fn submit<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        item: QueueEntry<T>,
        wait: W,
        ring: R,
        flags: SubmissionFlags,
    ) -> Result<(), QueueError> {
        let h = self.hdr().reserve_slot(flags, wait)?;
        let buf_item = self.get_buf(h as usize);

        // Write these manually, to ensure we do not set cmd_slot until the end.
        unsafe { *addr_of_mut!((*buf_item).data) = item.data };
        unsafe { *addr_of_mut!((*buf_item).info) = item.info };
        let turn = self.hdr().get_turn(h);
        unsafe { QueueEntry::set_cmd_slot(buf_item, h | if turn { 1u32 << 31 } else { 0 }) };

        self.hdr().ring(ring);
        Ok(())
    }

    /// Receive data from the queue, returning either that data or an error. The wait and ring
    /// callbacks work similar to [RawQueue::submit].
    pub fn receive<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        wait: W,
        ring: R,
        flags: ReceiveFlags,
    ) -> Result<QueueEntry<T>, QueueError> {
        self.receive_spin(SPIN_ATTEMPTS, wait, ring, flags)
            .map(|(item, _)| item)
    }

    /// [`RawQueue::receive`] with an explicit spin budget, also reporting how much of it was spent.
    ///
    /// [SPIN_ATTEMPTS] is sized for a consumer whose producer is generally running, where paying a
    /// spin to dodge a park is a good trade. A consumer whose producer answers one request at a
    /// time gets the opposite deal: the queue is empty every time it comes back, the whole budget
    /// is spent, and it parks anyway -- so the spin buys nothing and costs its full length on every
    /// drain. The spend is reported rather than merely bounded because the two cases are only
    /// distinguishable at runtime: a budget consumed in full and followed by a park is the failing
    /// one, and a caller that can see that can size itself to it.
    pub fn receive_spin<W: Fn(&AtomicU64, u64), R: Fn(&AtomicU64)>(
        &self,
        spin_attempts: usize,
        wait: W,
        ring: R,
        flags: ReceiveFlags,
    ) -> Result<(QueueEntry<T>, usize), QueueError> {
        let (t, spun) = self
            .hdr()
            .get_next_ready(spin_attempts, wait, flags, unsafe { *self.buf.get() })?;
        let buf_item = self.get_buf(t as usize);
        let item = unsafe { buf_item.read() };
        self.hdr().advance_tail(ring);
        Ok((item, spun))
    }

    pub fn has_pending(&self) -> bool {
        self.hdr().has_pending(unsafe { *self.buf.get() })
    }

    /// See [`RawQueueHdr::pending_parts`].
    pub fn pending_parts(&self) -> (u64, u64, bool, bool) {
        self.hdr().pending_parts(unsafe { *self.buf.get() })
    }

    pub fn has_space(&self) -> bool {
        self.hdr().has_space::<T>()
    }

    pub fn setup_sleep<'a>(
        &'a self,
        sleep: bool,
        output: &mut Option<QueueEntry<T>>,
        waiter: &mut (Option<&'a AtomicU64>, u64),
        ringer: &mut Option<&'a AtomicU64>,
    ) -> Result<(), QueueError> {
        let t = self
            .hdr()
            .setup_rec_sleep(sleep, unsafe { *self.buf.get() }, waiter)?;
        let buf_item = self.get_buf(t as usize);
        let item = unsafe { buf_item.read() };
        *output = Some(item);
        self.hdr().advance_tail_setup(ringer);
        Ok(())
    }

    #[inline]
    pub fn setup_sleep_simple(&self) -> (&AtomicU64, u64) {
        self.hdr().setup_rec_sleep_simple()
    }

    #[inline]
    pub fn setup_send_sleep_simple(&self) -> (&AtomicU64, u64) {
        self.hdr().setup_send_sleep_simple()
    }
}

unsafe impl<T: Send> Send for RawQueue<T> {}
unsafe impl<T: Send> Sync for RawQueue<T> {}

#[cfg(any(feature = "std", test))]
/// Wait for receiving on multiple raw queues. If any of the passed raw queues can return data, they
/// will do so by writing it into the output array at the same index that they are in the `queues`
/// variable. The queues and output arrays must be the same length. If no data is available in any
/// queues, then the function will call back on multi_wait, which it expects to wait until **any**
/// of the pairs (&x, y) meet the condition that *x != y. Before returning any data, the function
/// will callback on multi_ring, to inform multiple queues that data was taken from them. It expects
/// the multi_ring function to wake up any waiting threads on the supplied words of memory.
///
/// Note that both call backs specify the pointers as Option. In the case that an entry is None,
/// there was no requested wait or wake operation for that queue, and that entry should be ignored.
///
/// If flags specifies [ReceiveFlags::NON_BLOCK], then if no data is available, the function returns
/// immediately with Err([QueueError::WouldBlock]).
///
/// # Rationale
/// This function is here to implement poll or select like functionality, wherein a given thread or
/// program wants to wait on multiple incoming request channels and handle them itself, thus cutting
/// down on the number of threads required. The maximum number of queues to use here is a trade-off
/// --- more means fewer threads, but since this function is linear in the number of queues, each
/// thread could take longer to service requests.
///
/// The complexity of the multi_wait and multi_ring callbacks is present to avoid calling into the
/// kernel often for high-contention queues.
pub fn multi_receive<T: Copy, W: Fn(&[(Option<&AtomicU64>, u64)]), R: Fn(&[Option<&AtomicU64>])>(
    queues: &[&RawQueue<T>],
    output: &mut [Option<QueueEntry<T>>],
    multi_wait: W,
    multi_ring: R,
    flags: ReceiveFlags,
) -> Result<usize, QueueError> {
    if output.len() != queues.len() {
        return Err(QueueError::Unknown);
    }
    // Both scratch vectors stay empty until a queue actually produces something to ring or we are
    // about to sleep; the common case is a ready queue on the first pass, which allocates once for
    // the ringers and never touches the waiters.
    let mut waiters: Vec<(Option<&AtomicU64>, u64)> = Vec::new();
    let mut ringers: Vec<Option<&AtomicU64>> = Vec::new();
    let mut attempts = 100;
    loop {
        let sleep = attempts == 0;
        let mut count = 0;
        for (i, q) in queues.iter().enumerate() {
            let mut waiter = Default::default();
            let mut ringer = None;
            if q.setup_sleep(sleep, &mut output[i], &mut waiter, &mut ringer) == Ok(()) {
                count += 1;
            }
            if ringer.is_some() {
                ringers.resize(queues.len(), None);
                ringers[i] = ringer;
            }
            if sleep {
                waiters.resize(queues.len(), Default::default());
                waiters[i] = waiter;
            }
        }
        if count > 0 {
            multi_ring(&ringers);
            return Ok(count);
        }
        if flags.contains(ReceiveFlags::NON_BLOCK) {
            return Err(QueueError::WouldBlock);
        }
        if attempts > 0 {
            attempts -= 1;
        } else {
            multi_wait(&waiters);
        }
    }
}

#[cfg(all(test, not(loom)))]
impl RawQueueHdr {
    /// Fast-forward the counters, so wrap behaviour can be exercised in microseconds instead of
    /// 2^31 real operations.
    fn seed(&self, head: u32, tail: u64, bell: u64) {
        self.producer.head.store(head, Ordering::SeqCst);
        // Keep the producers' cache consistent with the counter it shadows: it may lag `tail`, but
        // it must never lead it, or `has_room` would hand out a slot over live data.
        self.producer
            .cached_tail
            .store((tail & 0x7fffffff) as u32, Ordering::SeqCst);
        self.tail.store(tail, Ordering::SeqCst);
        self.bell.store(bell, Ordering::SeqCst);
    }
}

// Loom builds swap in loom's atomics, which panic if constructed outside a `loom::model` closure.
#[cfg(all(test, not(loom)))]
mod tests {
    #![allow(soft_unstable)]
    use std::sync::atomic::{AtomicU64, Ordering};

    //   use syscalls::SyscallArgs;
    use crate::multi_receive;
    use crate::{QueueEntry, QueueError, RawQueue, RawQueueHdr, ReceiveFlags, SubmissionFlags};

    fn wait(x: &AtomicU64, v: u64) {
        while x.load(Ordering::SeqCst) == v {
            core::hint::spin_loop();
        }
    }

    fn wake(_x: &AtomicU64) {
        //   println!("wake");
    }

    #[test]
    fn it_transmits() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        for i in 0..100 {
            let res = q.submit(
                QueueEntry::new(i as u32, i * 10),
                wait,
                wake,
                SubmissionFlags::empty(),
            );
            assert_eq!(res, Ok(()));
            let res = q.receive(wait, wake, ReceiveFlags::empty());
            assert!(res.is_ok());
            assert_eq!(res.unwrap().info(), i as u32);
            assert_eq!(res.unwrap().item(), i * 10);
        }
    }

    #[test]
    fn it_fills() {
        let qh = RawQueueHdr::new(2, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 2];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        let res = q.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(2, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(3, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(QueueEntry::new(4, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.submit(
            QueueEntry::new(1, 7),
            wait,
            wake,
            SubmissionFlags::NON_BLOCK,
        );
        assert_eq!(res, Err(QueueError::WouldBlock));
    }

    /// An async submitter arms a `ThreadSyncSleep` on `tail` and returns rather than blocking, so
    /// it has to register itself for the consumer to ring. It used to not, and the sleep was never
    /// woken.
    #[test]
    fn it_wakes_async_submitters() {
        let qh = RawQueueHdr::new(2, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 2];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        for i in 0..4 {
            let res = q.submit(QueueEntry::new(i, 7), wait, wake, SubmissionFlags::empty());
            assert_eq!(res, Ok(()));
        }
        let res = q.submit(
            QueueEntry::new(5, 7),
            wait,
            wake,
            SubmissionFlags::NON_BLOCK,
        );
        assert_eq!(res, Err(QueueError::WouldBlock));

        let _armed = q.setup_send_sleep_simple();

        let rings = AtomicU64::new(0);
        let count = |_: &AtomicU64| {
            rings.fetch_add(1, Ordering::SeqCst);
        };

        assert!(q.receive(wait, &count, ReceiveFlags::NON_BLOCK).is_ok());
        assert_eq!(
            rings.load(Ordering::SeqCst),
            1,
            "freeing a slot must ring the tail for an armed async submitter"
        );

        // The registration is consumed by that ring, so a queue nobody is waiting on does not pay
        // for a wake on every dequeue.
        assert!(q.receive(wait, &count, ReceiveFlags::NON_BLOCK).is_ok());
        assert_eq!(rings.load(Ordering::SeqCst), 1);
    }

    /// The producers' cached `tail` is only allowed to be pessimistic. Once it says full it must be
    /// refreshed against the real counter, or a queue that the consumer has since drained stays
    /// full forever from the producer's point of view — a hang, not a slowdown.
    #[test]
    fn producer_cache_refreshes_after_drain() {
        let qh = RawQueueHdr::new(2, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 2];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        for i in 0..4 {
            q.submit(QueueEntry::new(i, 7), wait, wake, SubmissionFlags::empty())
                .unwrap();
        }
        // Cache is now stale-and-full: this is the load that populates it.
        assert_eq!(
            q.submit(
                QueueEntry::new(9, 7),
                wait,
                wake,
                SubmissionFlags::NON_BLOCK
            ),
            Err(QueueError::WouldBlock)
        );

        assert!(q.receive(wait, wake, ReceiveFlags::NON_BLOCK).is_ok());
        assert_eq!(
            q.submit(
                QueueEntry::new(9, 7),
                wait,
                wake,
                SubmissionFlags::NON_BLOCK
            ),
            Ok(()),
            "a full-looking cache must be rechecked against the real tail"
        );
    }

    /// Sanity on the layout the cross-core numbers depend on: the counters written by different
    /// threads must land on different cache lines. Cheap to assert, and silent to lose.
    #[test]
    fn header_counters_do_not_share_cache_lines() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let line = |p: *const u8| (p as usize) / 64;

        let head = line(&qh.producer.head as *const _ as *const u8);
        let cached = line(&qh.producer.cached_tail as *const _ as *const u8);
        let waiters = line(&*qh.waiters as *const _ as *const u8);
        let cwait = line(&*qh.consumer_waiting as *const _ as *const u8);
        let bell = line(&*qh.bell as *const _ as *const u8);
        let tail = line(&*qh.tail as *const _ as *const u8);

        assert_eq!(
            head, cached,
            "both are producer-owned; sharing a line is the point"
        );
        for (a, b) in [
            (head, waiters),
            (head, bell),
            (head, tail),
            (waiters, bell),
            (waiters, tail),
            (bell, tail),
            (cwait, bell),
            (cwait, tail),
        ] {
            assert_ne!(a, b, "cross-thread counters must not share a cache line");
        }
    }

    #[test]
    fn it_nonblock_receives() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        let res = q.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q.receive(wait, wake, ReceiveFlags::empty());
        assert!(res.is_ok());
        assert_eq!(res.unwrap().info(), 1);
        assert_eq!(res.unwrap().item(), 7);
        let res = q.receive(wait, wake, ReceiveFlags::NON_BLOCK);
        assert_eq!(res.unwrap_err(), QueueError::WouldBlock);
    }

    /// Arming a sleep must be honest: the returned value has to be the current contents of the
    /// returned word, or the caller's `sys_thread_sync` returns immediately and the reactor spins
    /// instead of sleeping. Both `setup_*_sleep_simple` functions have shipped a violation of this.
    #[test]
    fn arming_to_receive_matches_the_bell() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let (word, value) = qh.setup_rec_sleep_simple();
        assert_eq!(word.load(Ordering::SeqCst), value);
    }

    /// As above, past 2^31 operations. `bell` free-runs while `tail` is only ever compared modulo
    /// 2^31, so a `tail`-derived sleep value stops matching `bell` here and every armed receive
    /// degenerates into a busy loop.
    #[test]
    fn arming_to_receive_matches_the_bell_past_wrap() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        qh.seed(0x8000_0010, 0x8000_0000, 0x8000_0010);
        let (word, value) = qh.setup_rec_sleep_simple();
        assert_eq!(word.load(Ordering::SeqCst), value);
    }

    /// As above for the send side, with the consumer-waiting bit set in the very word being slept
    /// on. Masking that bit out of the returned value is what made this spin.
    #[test]
    fn arming_to_send_matches_the_tail() {
        let qh = RawQueueHdr::new(0, std::mem::size_of::<QueueEntry<u32>>());
        qh.reserve_slot(SubmissionFlags::NON_BLOCK, |_, _| {})
            .unwrap();
        qh.consumer_set_waiting(true);

        let (word, value) = qh.setup_send_sleep_simple();
        assert_eq!(word.load(Ordering::SeqCst), value);
    }

    /// With space available there is nothing to wait for, so arming must produce a predicate that
    /// is already false rather than one that blocks.
    #[test]
    fn arming_to_send_with_space_does_not_sleep() {
        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let (word, value) = qh.setup_send_sleep_simple();
        assert_ne!(word.load(Ordering::SeqCst), value);
    }

    /// A real producer and consumer on two threads must deliver every item, in order, exactly once.
    ///
    /// The queue is deliberately tiny so the producer genuinely fills it and blocks: that is what
    /// exercises `reserve_slot`'s wait path, the `cached_tail` refresh, and `advance_tail`'s wake
    /// under actual contention rather than in a single thread's cache. Sized to finish in
    /// milliseconds — the throughput and latency measurements live in `benches/queue.rs`.
    #[test]
    fn two_threads_deliver_every_item_in_order() {
        const COUNT: u64 = 20_000;

        let qh = RawQueueHdr::new(3, std::mem::size_of::<QueueEntry<u64>>());
        let mut buffer = vec![QueueEntry::<u64>::default(); 1 << 3];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        std::thread::scope(|s| {
            let consumer = s.spawn(|| {
                for expect in 0..COUNT {
                    let e = q.receive(wait, wake, ReceiveFlags::empty()).unwrap();
                    assert_eq!(e.item(), expect, "item out of order or lost");
                    assert_eq!(e.info() as u64, expect % (u32::MAX as u64));
                }
                // Nothing left over.
                assert!(matches!(
                    q.receive(wait, wake, ReceiveFlags::NON_BLOCK),
                    Err(QueueError::WouldBlock)
                ));
            });

            for i in 0..COUNT {
                q.submit(
                    QueueEntry::new((i % (u32::MAX as u64)) as u32, i),
                    wait,
                    wake,
                    SubmissionFlags::empty(),
                )
                .unwrap();
            }
            consumer.join().unwrap();
        });
    }

    /// The uncontended path must not touch the callbacks at all: in the real queue each one is a
    /// `sys_thread_sync`, so a stray call is a syscall on the fast path.
    #[test]
    fn uncontended_path_makes_no_callbacks() {
        use std::cell::Cell;

        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<u32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        let (waits, rings) = (Cell::new(0), Cell::new(0));
        let wait = |_: &AtomicU64, _: u64| waits.set(waits.get() + 1);
        let ring = |_: &AtomicU64| rings.set(rings.get() + 1);

        for i in 0..100 {
            q.submit(QueueEntry::new(i, i), wait, ring, SubmissionFlags::empty())
                .unwrap();
            q.receive(wait, ring, ReceiveFlags::empty()).unwrap();
        }
        assert_eq!((waits.get(), rings.get()), (0, 0));
    }

    /// An abandoned arm -- a poller that registers interest and then drops it without ever being
    /// woken -- must cost a bounded number of stray wakes, not one per operation forever.
    #[test]
    fn abandoned_receive_arm_costs_at_most_one_wake() {
        use std::cell::Cell;

        let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<u32>::default(); 1 << 4];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        // Arm, then walk away without ever sleeping on it.
        let _ = qh.setup_rec_sleep_simple();

        let rings = Cell::new(0);
        let ring = |_: &AtomicU64| rings.set(rings.get() + 1);
        for i in 0..100 {
            q.submit(
                QueueEntry::new(i, i),
                |_, _| {},
                ring,
                SubmissionFlags::empty(),
            )
            .unwrap();
            q.receive(|_, _| {}, ring, ReceiveFlags::empty()).unwrap();
        }
        assert_eq!(rings.get(), 1);
    }

    /// As above for the send side, whose sticky bit is consumed by `advance_tail`.
    #[test]
    fn abandoned_send_arm_costs_at_most_one_wake() {
        use std::cell::Cell;

        let qh = RawQueueHdr::new(1, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer = [QueueEntry::<u32>::default(); 2];
        let q = unsafe { RawQueue::new(&qh, buffer.as_mut_ptr()) };

        q.submit(
            QueueEntry::new(0, 0),
            |_, _| {},
            |_| {},
            SubmissionFlags::empty(),
        )
        .unwrap();
        q.submit(
            QueueEntry::new(1, 1),
            |_, _| {},
            |_| {},
            SubmissionFlags::empty(),
        )
        .unwrap();
        // Full: arm for space, then walk away.
        let _ = qh.setup_send_sleep_simple();

        let rings = Cell::new(0);
        let ring = |_: &AtomicU64| rings.set(rings.get() + 1);
        for i in 0..100 {
            q.receive(|_, _| {}, ring, ReceiveFlags::empty()).unwrap();
            q.submit(
                QueueEntry::new(i, i),
                |_, _| {},
                ring,
                SubmissionFlags::empty(),
            )
            .unwrap();
        }
        assert_eq!(rings.get(), 1);
    }

    #[test]
    fn it_multi_receives() {
        let qh1 = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer1 = [QueueEntry::<i32>::default(); 1 << 4];
        let q1 = unsafe { RawQueue::new(&qh1, buffer1.as_mut_ptr()) };

        let qh2 = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
        let mut buffer2 = [QueueEntry::<i32>::default(); 1 << 4];
        let q2 = unsafe { RawQueue::new(&qh2, buffer2.as_mut_ptr()) };

        let res = q1.submit(QueueEntry::new(1, 7), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));
        let res = q2.submit(QueueEntry::new(2, 8), wait, wake, SubmissionFlags::empty());
        assert_eq!(res, Ok(()));

        let mut output = [None, None];
        let res = multi_receive(
            &[&q1, &q2],
            &mut output,
            |_| {},
            |_| {},
            ReceiveFlags::empty(),
        );
        assert_eq!(res, Ok(2));
        assert_eq!(output[0].unwrap().info(), 1);
        assert_eq!(output[0].unwrap().item(), 7);
        assert_eq!(output[1].unwrap().info(), 2);
        assert_eq!(output[1].unwrap().item(), 8);
    }

    /*
        #[cfg(not(target_os = "twizzler"))]
        extern crate crossbeam;
        #[cfg(not(target_os = "twizzler"))]
        extern crate test;
        #[cfg(not(target_os = "twizzler"))]
        #[bench]
        fn two_threads(b: &mut test::Bencher) -> impl Termination {
            let qh = RawQueueHdr::new(4, std::mem::size_of::<QueueEntry<u32>>());
            let mut buffer = [QueueEntry::<i32>::default(); 1 << 4];
            let q = unsafe {
                RawQueue::new(
                    std::mem::transmute::<&RawQueueHdr, &'static RawQueueHdr>(&qh),
                    buffer.as_mut_ptr(),
                )
            };

            //let count = AtomicU64::new(0);
            let x = crossbeam::scope(|s| {
                s.spawn(|_| loop {
                    let res = q.receive(wait, wake, ReceiveFlags::empty());
                    assert!(res.is_ok());
                    if res.unwrap().info() == 2 {
                        break;
                    }
                    //count.fetch_add(1, Ordering::SeqCst);
                });

                b.iter(|| {
                    let res = q.submit(QueueEntry::new(1, 2), wait, wake, SubmissionFlags::empty());
                    assert_eq!(res, Ok(()));
                });
                let res = q.submit(QueueEntry::new(2, 2), wait, wake, SubmissionFlags::empty());
                assert_eq!(res, Ok(()));
            });

            x.unwrap();
        }
    */
}
