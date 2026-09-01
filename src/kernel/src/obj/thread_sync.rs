use alloc::{boxed::Box, collections::BTreeMap, sync::Arc, vec::Vec};
use core::{
    ptr::NonNull,
    sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering},
};

use heapless::index_map::FnvIndexMap;
use intrusive_collections::{Adapter, KeyAdapter, RBTree, RBTreeAtomicLink, container_of, rbtree};
use twizzler_abi::{
    device::NUM_DEVICE_INTERRUPTS,
    object::ObjID,
    syscall::{ThreadSyncFlags, ThreadSyncOp},
};
use twizzler_rt_abi::error::TwzError;

use super::{OBJ_HAS_INTERRUPTS, Object};
use crate::{
    interrupt::{remove_from_device_wait, wait_for_device_interrupt},
    obj::ObjectRef,
    syscall::sync::add_to_requeue,
    thread::{Thread, ThreadRef, current_thread_ref},
};

struct SleepLinkNode {
    link: RBTreeAtomicLink,
    owner: Arc<Thread>,
}

pub struct ThreadSleepLinker {
    links: AtomicPtr<Box<[SleepLinkNode]>>,
    next: AtomicUsize,
}

impl Drop for ThreadSleepLinker {
    fn drop(&mut self) {
        let links = self.links.swap(core::ptr::null_mut(), Ordering::SeqCst);
        if links.is_null() {
            return;
        }
        let links = unsafe { Box::from_raw(links) };
        drop(links);
    }
}

impl ThreadSleepLinker {
    pub fn new() -> Self {
        Self {
            links: AtomicPtr::new(Box::into_raw(Box::new(Box::new([])))),
            next: AtomicUsize::new(0),
        }
    }

    fn get_links(&self) -> &Box<[SleepLinkNode]> {
        if self.links.load(Ordering::SeqCst).is_null() {
            panic!("ThreadSleepLinker: get_links called after clear_all_references");
        }
        unsafe { &*self.links.load(Ordering::SeqCst) }
    }

    pub fn len(&self) -> usize {
        self.get_links().len()
    }

    pub fn reserve(&self, count: usize, thread: &ThreadRef) {
        assert!(
            self.next.load(Ordering::SeqCst) == 0,
            "ThreadSleepLinker::reserve called mid-round"
        );
        assert!(&thread.sync_links as *const _ == self as *const _);
        let old = self.links.load(Ordering::SeqCst);
        if self.len() < count {
            let mut new_links = Vec::with_capacity(count);
            for _ in 0..count {
                new_links.push(SleepLinkNode {
                    link: RBTreeAtomicLink::new(),
                    owner: thread.clone(),
                });
            }
            let new_links = new_links.into_boxed_slice();
            let new_links_ptr = Box::into_raw(Box::new(new_links));

            if self
                .links
                .compare_exchange(old, new_links_ptr, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                drop(unsafe { Box::from_raw(new_links_ptr) });
                return self.reserve(count, thread);
            }

            // Freeing the old slabs is only safe if nothing still points at them. A link left in
            // some RBTree past the end of its round -- which `reset` reports rather than repairs,
            // because a link cannot name the tree it is in -- would become a dangling node the next
            // insert or removal walks. Leak the allocation instead: bounded by how often that
            // happens, and a leak is recoverable where the use-after-free is not.
            let stale = unsafe { &*old }.iter().any(|node| node.link.is_linked());
            if stale {
                log::warn!("ThreadSleepLinker::reserve: old links still linked, leaking them");
                core::mem::forget(unsafe { Box::from_raw(old) });
            } else {
                drop(unsafe { Box::from_raw(old) });
            }
        }
    }

    /// Returns the next available link slot, stamping it with `owner` so
    /// that [ThreadSleepAdapter::get_value] can later find its way back to
    /// the owning thread. Only ever called from `ThreadSleepAdapter`.
    fn get_link(&self) -> (&RBTreeAtomicLink, usize) {
        let links = self.get_links();
        let idx = self.next.fetch_add(1, Ordering::SeqCst);
        if idx >= links.len() {
            panic!("ThreadSleepLinker: not enough reserved capacity, call reserve() first");
        }
        (&links[idx].link, idx)
    }

    /// End of round: every slot handed out this `sys_thread_sync` should have been removed from
    /// whatever tree it was inserted into.
    ///
    /// A slot still linked here is *the* invariant break behind the "already linked" panic in
    /// `RBTree::insert`: `next` goes back to zero, the next round hands slot 0 out again, and the
    /// insert trips over a node that is still in a tree. The panic therefore fires a round late and
    /// names the innocent inserter -- which is why suppressing it at the insert (tried once in
    /// `setup_device_wait`, see the note there) hides the cause and makes things worse.
    ///
    /// This is the point that knows. It cannot repair anything -- a link cannot name the tree
    /// holding it -- but it can say so loudly enough to reach a transcript, which the `log::warn!`
    /// it used to carry did not.
    pub fn reset(&self) {
        let n = self.next.load(Ordering::SeqCst);
        for i in 0..n {
            let links = self.get_links();
            let link = &links[i].link;
            if link.is_linked() && crate::thread::locktrack::diag::SLEEP_LINK_LEAKED.hit() {
                emerglogln!(
                    "ThreadSleepLinker::reset: thread {} left sleep link {} of {} linked; the next round's insert on this slot will panic",
                    links[i].owner.objid(),
                    i,
                    n,
                );
            }
        }
        self.next.store(0, Ordering::SeqCst);
    }

    /// True if any slot is currently linked into some `RBTree`.
    pub fn is_linked(&self) -> bool {
        self.next.load(Ordering::SeqCst) > 0
    }

    /// Called from `Thread::exit`. This is `reserve`'s hazard on the exit path: a thread that
    /// leaves a slot in some object's sleep tree -- a force-exit out of a sleep never runs
    /// `undo_sleep`, and nothing else unlinks it -- turns this free into a dangling node that the
    /// next `wake_n` on that word walks, whose `owner` then reaches `add_to_requeue` as a garbage
    /// `ThreadRef`. Leak the slab instead, for the reason `reserve` gives: a leak is bounded by how
    /// often this happens, and the use-after-free is not recoverable.
    pub fn clear_all_references(&self) {
        let links = self.links.swap(core::ptr::null_mut(), Ordering::SeqCst);
        if links.is_null() {
            return;
        }
        let stale = unsafe { &*links }.iter().any(|node| node.link.is_linked());
        if stale {
            if crate::thread::locktrack::diag::SLEEP_LINK_LEAKED_AT_EXIT.hit() {
                emerglogln!(
                    "ThreadSleepLinker::clear_all_references: thread exiting with a sleep link still linked; leaking its slab",
                );
            }
            core::mem::forget(unsafe { Box::from_raw(links) });
        } else {
            drop(unsafe { Box::from_raw(links) });
        }
    }
}

#[derive(Clone)]
pub struct ThreadSleepAdapter {
    link_ops: rbtree::AtomicLinkOps,
    pointer_ops: intrusive_collections::DefaultPointerOps<ThreadRef>,
}

impl ThreadSleepAdapter {
    pub const NEW: Self = Self {
        link_ops: rbtree::AtomicLinkOps,
        pointer_ops: intrusive_collections::DefaultPointerOps::new(),
    };

    pub fn new() -> Self {
        Self::NEW
    }
}

impl Default for ThreadSleepAdapter {
    fn default() -> Self {
        Self::NEW
    }
}

unsafe impl Adapter for ThreadSleepAdapter {
    type LinkOps = rbtree::AtomicLinkOps;
    type PointerOps = intrusive_collections::DefaultPointerOps<ThreadRef>;

    unsafe fn get_value(&self, link: NonNull<RBTreeAtomicLink>) -> *const Thread {
        unsafe {
            let node = container_of!(link.as_ptr(), SleepLinkNode, link);
            let arc = &(*node).owner;
            Arc::as_ptr(arc)
        }
    }

    unsafe fn get_link(&self, value: *const Thread) -> NonNull<RBTreeAtomicLink> {
        unsafe {
            let thread = &*value;
            let (link, _idx) = thread.sync_links.get_link();
            NonNull::from(link)
        }
    }

    fn link_ops(&self) -> &Self::LinkOps {
        &self.link_ops
    }

    fn link_ops_mut(&mut self) -> &mut Self::LinkOps {
        &mut self.link_ops
    }

    fn pointer_ops(&self) -> &Self::PointerOps {
        &self.pointer_ops
    }
}

impl<'a> KeyAdapter<'a> for ThreadSleepAdapter {
    type Key = ObjID;

    fn get_key(&self, t: &'a Thread) -> ObjID {
        t.objid()
    }
}

/// Waiters claimed per pass of [Object::wakeup_word]. Sized so the overwhelmingly common wake --
/// one or a handful of threads off a queue doorbell -- takes a single pass and never re-locks.
const WAKE_BATCH: usize = 16;

/// Threads claimed under `sleep_info` and scheduled after it is dropped.
type WakeBatch = heapless::Vec<ThreadRef, WAKE_BATCH>;

struct SleepEntry {
    of_obj: ObjID,
    threads: RBTree<ThreadSleepAdapter>,
}

impl SleepEntry {
    pub fn new(thread: ThreadRef, of_obj: ObjID) -> Self {
        let mut threads = RBTree::new(ThreadSleepAdapter::NEW);
        threads.insert(thread);
        Self { threads, of_obj }
    }

    /// Returns whether the thread was actually added, which is what `Object::sleepers` counts --
    /// a duplicate must not be counted twice or the object never reaches zero again.
    pub fn add_thread(&mut self, thread: ThreadRef) -> bool {
        // If already on this list, skip -- mirrors do_add_to_requeue's
        // find()+insert() guard in syscall/sync.rs; protected by the
        // caller's sleep_info lock, so no TOCTOU race.
        if !self.threads.find(&thread.objid()).is_null() {
            return false;
        }
        self.threads.insert(thread);
        true
    }

    /// Returns whether an entry was actually removed; see [SleepEntry::add_thread].
    pub fn remove_thread(&mut self, id: ObjID) -> bool {
        let mut cursor = self.threads.find_mut(&id);
        cursor.remove().is_some()
    }

    /// Claim up to `max_count` waiters into `batch`, stopping early if it fills. Returns whether
    /// the batch filled, i.e. whether there may be more to claim on another pass.
    ///
    /// Claiming is `reset_sync_sleep` plus the removal, both under the caller's `sleep_info` lock;
    /// scheduling the claimed threads is [Object::wakeup_word]'s job, once that lock is gone.
    fn claim_n(&mut self, max_count: usize, batch: &mut WakeBatch, skipped: &mut usize) -> bool {
        let mut cursor = self.threads.front_mut();
        while !batch.is_full() && batch.len() < max_count && !cursor.is_null() {
            let thread = cursor.get().unwrap();
            if thread.reset_sync_sleep() {
                thread.note_sync_consumer(1);
                let thread = cursor.remove().unwrap();
                // Safety: not full, checked above.
                unsafe { batch.push_unchecked(thread) };
            } else {
                // A linked entry whose SYNC_SLEEP flag was already consumed. Transiently benign
                // (a concurrent wake on another of the thread's words won the flag first), but a
                // wake that claims nobody *because of these* is the lost-wake fingerprint the
                // caller reports -- see `wakeup_word`.
                *skipped += 1;
                cursor.move_next();
            }
        }
        batch.is_full()
    }
}

impl Drop for SleepEntry {
    fn drop(&mut self) {
        let mut cursor = self.threads.front_mut();
        while !cursor.is_null() {
            let thread = cursor.remove().unwrap();
            if thread.reset_sync_sleep() {
                thread.note_sync_consumer(2);
                add_to_requeue(thread);
            }
        }
        self.threads.fast_clear();
    }
}

pub struct SleepInfo {
    of_obj: ObjID,
    some_words: FnvIndexMap<usize, SleepEntry, 32>,
    more_words: Option<BTreeMap<usize, SleepEntry>>,
}

impl SleepInfo {
    pub fn new(of_obj: ObjID) -> Self {
        SleepInfo {
            some_words: FnvIndexMap::new(),
            more_words: None,
            of_obj,
        }
    }

    fn word(&mut self, offset: usize) -> Option<&mut SleepEntry> {
        if let Some(words) = self.more_words.as_mut() {
            words.get_mut(&offset)
        } else {
            self.some_words.get_mut(&offset)
        }
    }

    /// Returns whether a thread was actually parked, for `Object::sleepers`. Note the overflow
    /// arm below drops up to `some_words`' whole capacity through `SleepEntry::drop`, waking those
    /// threads without reporting them -- deliberately, per the bias note on `Object::sleepers`: it
    /// leaves the count high for an object that has already gone pathological.
    pub fn insert(&mut self, offset: usize, thread: ThreadRef) -> bool {
        if let Some(se) = self.word(offset) {
            return se.add_thread(thread);
        } else {
            if let Some(words) = self.more_words.as_mut() {
                words.insert(offset, SleepEntry::new(thread, self.of_obj));
            } else {
                match self
                    .some_words
                    .insert(offset, SleepEntry::new(thread, self.of_obj))
                {
                    Ok(_) => {}
                    Err((_, se)) => {
                        log::debug!("overflowing sleep entries");
                        // Clear the old words, wake up all those threads.
                        self.some_words.clear();
                        let mw = self.more_words.get_or_insert(BTreeMap::new());
                        mw.insert(offset, se);
                    }
                }
            }
        }
        true
    }

    /// Returns whether an entry was actually removed; see [SleepEntry::add_thread].
    pub fn remove(&mut self, offset: usize, thread_id: ObjID) -> bool {
        if let Some(se) = self.word(offset) {
            se.remove_thread(thread_id)
        } else {
            false
        }
    }

    fn claim_n(
        &mut self,
        offset: usize,
        max_count: usize,
        batch: &mut WakeBatch,
        skipped: &mut usize,
    ) -> bool {
        if let Some(se) = self.word(offset) {
            se.claim_n(max_count, batch, skipped)
        } else {
            false
        }
    }
}

/// Wake-delivery accounting for the mode-A lost-wake hunt.
///
/// The userspace side established that net-srv parks on a genuinely empty ring and is never
/// resumed while the producer keeps ringing, and that the wake never reaches `add_to_requeue`
/// (zero `rq1` rows in any wedge round). That leaves exactly three outcomes inside `wakeup_word`,
/// and aggregate counters cannot tell them apart from userspace:
///
///   FASTSKIP -- `sleepers == 0`, so the wake returned without looking. If the wedged sleeper is
///               still linked, this is the fast path dropping a wake at the door.
///   NOCLAIM  -- sleepers were present but `claim_n` matched nothing at this offset: either the
///               waker and the sleeper disagree about the offset, or the entry is present with a
///               turn the consumer will not accept (invisible to `receive`, no wake can help).
///   CLAIMED  -- normal.
///
/// Recorded unconditionally (three relaxed adds on a hot path); only the *reporting* is gated, so
/// a sweep that forgot `--diag=wake` still leaves the counts behind for the summary.
pub static WAKE_CALLS: AtomicU64 = AtomicU64::new(0);
pub static WAKE_FASTSKIP: AtomicU64 = AtomicU64::new(0);
pub static WAKE_NOCLAIM: AtomicU64 = AtomicU64::new(0);
pub static WAKE_CLAIMED: AtomicU64 = AtomicU64::new(0);
/// Offset of the most recent FASTSKIP/NOCLAIM, so the kernel-side record can be joined against
/// net-srv's park word (its census reports `w=0x..001140`; the offset is the shared key).
pub static WAKE_LAST_SKIP_OFF: AtomicU64 = AtomicU64::new(u64::MAX);
pub static WAKE_LAST_NOCLAIM_OFF: AtomicU64 = AtomicU64::new(u64::MAX);

impl Object {
    /// Wake up to `count` threads sleeping on `offset`, returning how many were woken.
    ///
    /// Claim under `sleep_info`, schedule outside it. `add_to_requeue`'s fast path is
    /// `schedule_thread` -- a topology walk, a remote run queue lock and a wakeup IPI -- and
    /// running it per waiter under this spinlock put all of that in an interrupts-off region, on
    /// the path every `sys_thread_sync` wake in the system takes. It also held a per-object
    /// spinlock across a run queue lock, which fixes an ordering nothing states or enforces, and
    /// dropped `ThreadRef`s under it, which is the `Thread::drop` -> `IdCounter::release` ->
    /// sleeping-mutex wedge `remove_from_requeue` documents.
    ///
    /// Correctness of deferring: a claim is winning `reset_sync_sleep` *and* taking the entry, both
    /// done under the lock here, and every other path to scheduling one of these threads must win
    /// that same flag first. So a claimed thread is ours alone until we requeue it.
    ///
    /// Re-locking between passes is invisible to callers. A `count` at or below `WAKE_BATCH` --
    /// every bounded wake -- takes one pass and never unlocks mid-walk, so its semantics are
    /// unchanged outright. Above that, a sleeper cannot slip in between passes and take a wake
    /// meant for someone already queued: `setup_sleep_word` re-reads the word under this same lock
    /// and only inserts if the word still says sleep, and the waker writes the word before getting
    /// here.
    ///
    /// Critical across the handoff, and the guard cannot simply wrap the loop: `sleep_info` is a
    /// *sleeping* `Mutex`, not a `Spinlock`, and `Mutex::lock` panics outright in a critical
    /// context. It is taken while the lock still covers the claim and released before the next
    /// pass re-takes it, which leaves no instant in which a claimed waiter is exposed.
    ///
    /// It is needed because a claimed waiter is on no list at all -- off the sleep tree with its
    /// SYNC_SLEEP flag already consumed -- until `add_to_requeue` runs. This thread exiting at a
    /// poll point in that window (an interrupt return -> `schedule_maybe_preempt` ->
    /// `schedule(REINSERT)` -> `maybe_exit` -> `exit`, which never returns) takes the batch and
    /// those wakeups with it, and nothing recovers them: the hardtick backstop drains the requeue
    /// list, and a batched waiter never reached it. The drain this replaced was covered by
    /// accident -- it ran with `sleep_info` held, and `maybe_exit` defers while
    /// `get_mutex_count() > 0` -- so the batch is what opened the poll point, by doing the handoff
    /// after the unlock. Same guard, same reason, as the device-interrupt drain in `interrupt.rs`
    /// and the handoff in `Mutex::release`.
    pub fn wakeup_word(&self, offset: usize, count: usize) -> usize {
        // Nobody is parked anywhere on this object, so there is nothing here to do -- and finding
        // that out used to cost a full `sleep_info` acquire, on the path every uncontended futex
        // release in the system takes. See the ordering and bias notes on `Object::sleepers`.
        //
        // The fence is load-bearing, not decoration. The word this wake corresponds to was written
        // by *userspace* before it entered the kernel, so nothing here can assume that store is
        // `SeqCst`, and on x86 a `SeqCst` load is a plain `mov` that does not order against a
        // prior store. Without the fence the store-then-load half of the Dekker pair is missing
        // and a wake can read zero against a sleeper that is about to park. One `mfence` against a
        // sleeping-mutex acquire is a trade worth making.
        /// A/B arm selector; see entryperf.md, "§8 under suspicion". `false` sends every wake
        /// through the `sleep_info` mutex, i.e. pre-§8 behaviour.
        ///
        /// **The A/B has been run and this path is exonerated:** 10 boots per arm, and the arms
        /// were indistinguishable -- 10/10 slow either way, open-phase medians 12646 vs 12778 us,
        /// and the same ~5.8 cold lookups per boot over 2 ms. The open-phase regression that put
        /// this under suspicion is not caused by the skip. Kept as a toggle because this path is
        /// the first suspect whenever a lost wake is suspected, and re-running that experiment
        /// should cost one character rather than a reconstruction.
        ///
        /// The fence is inside the guard, not merely the load. It exists only to pair with that
        /// load, so with the skip off it has nothing to order against -- and leaving an `mfence` on
        /// every wake in the control arm would make it slower than the behaviour it is standing in
        /// for, biasing the comparison toward the skip. The counter is still maintained in both
        /// arms: only the read is removed, so every increment and decrement path stays exercised
        /// and a counting bug would still reach the drain assertion in
        /// `sleeper_count_wakes_and_drains`.
        // Provenance, not a live arm: this was flipped to `false` from 2026-08-31 23:55 to
        // 09-01 00:01 for the mode-A lost-wake A/B the const's own docs invite. The masters of
        // sweep `many-noskip` carry `false`; every other build, before and after that window,
        // carries `true`. Recorded because absolute numbers from that sweep are not comparable
        // to any other, and because a reader who greps this const should be able to tell which
        // images have which arm without reconstructing it from mtimes.
        const SLEEPERS_SKIP: bool = true;
        // Periodic totals on a fixed stride, not milestones: the question this instrument exists
        // to answer is a *ratio* (how many wakes bounced against how many landed), and a report
        // that only appears when a count is non-zero cannot state the denominator.
        let calls = WAKE_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
        if calls % (1 << 18) == 0 && crate::kdiag_wake() {
            logln!(
                "WAKESTAT calls={} claimed={} fastskip={} noclaim={} last_skip_off={:x} last_noclaim_off={:x}",
                calls,
                WAKE_CLAIMED.load(Ordering::Relaxed),
                WAKE_FASTSKIP.load(Ordering::Relaxed),
                WAKE_NOCLAIM.load(Ordering::Relaxed),
                WAKE_LAST_SKIP_OFF.load(Ordering::Relaxed),
                WAKE_LAST_NOCLAIM_OFF.load(Ordering::Relaxed),
            );
        }
        if SLEEPERS_SKIP {
            core::sync::atomic::fence(Ordering::SeqCst);
            if self.sleepers.load(Ordering::SeqCst) == 0 {
                // Counted before the return: a wake that leaves here woke nobody, and whether
                // that was correct depends on whether anyone was still linked -- which only the
                // sleeper side can say. The offset is the join key against that side.
                let n = WAKE_FASTSKIP.fetch_add(1, Ordering::Relaxed) + 1;
                WAKE_LAST_SKIP_OFF.store(offset as u64, Ordering::Relaxed);
                if n.is_power_of_two() && crate::kdiag_wake() {
                    logln!("WAKESKIP off={:x} n={} (sleepers==0 at the door)", offset, n);
                }
                return 0;
            }
        }
        let mut woken = 0;
        let mut skipped = 0usize;
        while woken < count {
            let mut batch = WakeBatch::new();
            let maybe_more;
            // Declared after `batch` so it drops first: see the requeue loop below.
            let _critical = {
                let Some(si) = self.sleep_info_if_present() else {
                    break;
                };
                let mut sleep_info = si.lock();
                maybe_more = sleep_info.claim_n(offset, count - woken, &mut batch, &mut skipped);
                current_thread_ref().map(|ct| ct.enter_critical())
            };
            if batch.is_empty() {
                break;
            }
            // Claimed under the lock above, so these are ours alone and cannot be double-counted.
            self.sleepers.fetch_sub(batch.len(), Ordering::SeqCst);
            woken += batch.len();
            // Cloned rather than moved, so `batch` outlives `_critical` and no reference can reach
            // zero inside the guard: `add_to_requeue`'s fast path hands the reference to
            // `schedule_thread`, which drops it for an exiting thread, and that last drop reaches
            // `IdCounter::release` -- a sleeping mutex, and `Mutex::lock` panics when critical.
            for thread in &batch {
                add_to_requeue(thread.clone());
            }
            // A short batch means the walk reached the end of the sleep entry or ran out of
            // `count`; only a full one says there may be more waiting.
            if !maybe_more {
                break;
            }
        }
        // A wake that claimed nobody while threads sat linked on exactly this word is the
        // lost-wake fingerprint: each such thread's SYNC_SLEEP flag was already consumed, so no
        // wake can ever claim it again and it sleeps until something re-files it. Split from the
        // benign silent return (empty tree) so a transcript can tell "the wake was issued and
        // bounced" from "no wake was ever issued". Rate-limited on powers of two.
        // Outcome accounting, complementing the bounced case below rather than duplicating it.
        // `woken == 0 && skipped == 0` is the arm nothing currently records: the wake reached the
        // kernel, took the sleep_info lock, and found *nothing linked at this offset* -- which is
        // either a waker/sleeper offset disagreement or a sleeper that is registered elsewhere.
        // Distinguishing it from `skipped > 0` (linked but unclaimable) matters because they
        // indict different code: the former the offset resolution, the latter the turn protocol.
        if woken > 0 {
            WAKE_CLAIMED.fetch_add(1, Ordering::Relaxed);
        } else {
            let n = WAKE_NOCLAIM.fetch_add(1, Ordering::Relaxed) + 1;
            WAKE_LAST_NOCLAIM_OFF.store(offset as u64, Ordering::Relaxed);
            if skipped == 0 && n.is_power_of_two() && crate::kdiag_wake() {
                logln!(
                    "WAKENOENT off={:x} n={} (wake found nothing linked at this offset)",
                    offset,
                    n
                );
            }
        }
        if woken == 0 && skipped > 0 {
            static CLAIM_BOUNCED: core::sync::atomic::AtomicU64 =
                core::sync::atomic::AtomicU64::new(0);
            let n = CLAIM_BOUNCED.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_power_of_two() {
                emerglogln!(
                    "[wake] claim bounced: 0 of {} linked sleeper(s) claimable at {}+{:x} (n={})",
                    skipped,
                    self.id(),
                    offset,
                    n
                );
            }
        }
        woken
    }

    pub fn add_device_interrupt(&self, vector: u32, num: usize, offset: usize) {
        let info = self.device_interrupt_table();
        info[num].0.store(vector as u64, Ordering::Release);
        info[num].1.store(offset as u64, Ordering::Release);
        // After the stores, and it is the publication edge: every reader below is gated on this
        // flag, so setting it first would let one reach a table that does not exist yet.
        self.flags.fetch_or(OBJ_HAS_INTERRUPTS, Ordering::Release);
    }

    pub fn setup_sleep_word(
        self: &ObjectRef,
        offset: usize,
        op: ThreadSyncOp,
        val: u64,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU64>,
    ) -> Result<bool, TwzError> {
        let thread = current_thread_ref().unwrap();

        if let Some(vaddr) = vaddr {
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return Ok(false);
            }
            if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
                let info = self.device_interrupt_table();
                for i in 0..NUM_DEVICE_INTERRUPTS {
                    let di_offset = info[i].1.load(Ordering::Acquire);
                    let di_vector = info[i].0.load(Ordering::Acquire);
                    if di_offset as usize == offset {
                        return Ok(wait_for_device_interrupt(
                            thread,
                            di_vector as u32,
                            first_sleep,
                            vaddr,
                        ));
                    }
                }
            }
        }

        // Claim before the authoritative word read below, not after the insert. This is the
        // sleeper half of the Dekker pair described on `Object::sleepers`, and the order is the
        // correctness argument: a waker that reads zero is then ordered ahead of this increment,
        // hence ahead of the read, so we observe its store and decline to sleep. Claiming after
        // the read inverts the pair and loses the wake.
        self.sleepers.fetch_add(1, Ordering::SeqCst);
        let mut sleep_info = self.sleep_info().lock();
        let cur = match vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_64(offset))
        {
            Ok(cur) => cur,
            // Release the claim rather than leaking it: a bad offset is a repeatable userspace
            // error, so leaking here would disable the fast path for this object permanently.
            Err(e) => {
                drop(sleep_info);
                self.sleepers.fetch_sub(1, Ordering::SeqCst);
                return Err(e);
            }
        };
        let res = op.check(cur, val, flags);
        log::trace!(
            "thread {} ({}) setting sleep word on {} (did sleep? {})",
            thread.id(),
            thread.objid(),
            self.id(),
            res,
        );
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            // A duplicate park returns false and is not a second sleeper.
            if !sleep_info.insert(offset, thread.clone()) {
                self.sleepers.fetch_sub(1, Ordering::SeqCst);
            }
        } else {
            self.sleepers.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(res)
    }

    pub fn setup_sleep_word32(
        self: &ObjectRef,
        offset: usize,
        op: ThreadSyncOp,
        val: u32,
        first_sleep: bool,
        flags: ThreadSyncFlags,
        vaddr: Option<&AtomicU32>,
    ) -> Result<bool, TwzError> {
        let thread = current_thread_ref().unwrap();
        if let Some(vaddr) = vaddr {
            let cur = vaddr.load(Ordering::SeqCst);
            if !op.check(cur, val, flags) {
                return Ok(false);
            }
        }
        // Claim before the authoritative word read below, not after the insert. This is the
        // sleeper half of the Dekker pair described on `Object::sleepers`, and the order is the
        // correctness argument: a waker that reads zero is then ordered ahead of this increment,
        // hence ahead of the read, so we observe its store and decline to sleep. Claiming after
        // the read inverts the pair and loses the wake.
        self.sleepers.fetch_add(1, Ordering::SeqCst);
        let mut sleep_info = self.sleep_info().lock();

        let cur = match vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_32(offset))
        {
            Ok(cur) => cur,
            Err(e) => {
                drop(sleep_info);
                self.sleepers.fetch_sub(1, Ordering::SeqCst);
                return Err(e);
            }
        };
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            if !sleep_info.insert(offset, thread.clone()) {
                self.sleepers.fetch_sub(1, Ordering::SeqCst);
            }
        } else {
            self.sleepers.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(res)
    }

    pub fn remove_from_sleep_word(&self, offset: usize) {
        let thread = current_thread_ref().unwrap();
        if let Some(si) = self.sleep_info_if_present() {
            let mut sleep_info = si.lock();
            // Only on a real removal: a word this thread was already woken off (claimed by
            // `wakeup_word`, or drained by an overflow) is gone, and was decremented there.
            if sleep_info.remove(offset, thread.objid()) {
                self.sleepers.fetch_sub(1, Ordering::SeqCst);
            }
        }

        // TODO: I think this only works if the thread waits on one interrupt.
        if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
            let info = self.device_interrupt_table();
            for i in 0..NUM_DEVICE_INTERRUPTS {
                let di_offset = info[i].1.load(Ordering::Acquire);
                let di_vector = info[i].0.load(Ordering::Acquire);
                if di_offset as usize == offset {
                    remove_from_device_wait(thread, di_vector as u32);
                    break;
                }
            }
        }
    }
}
