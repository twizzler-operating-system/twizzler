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

    pub fn add_thread(&mut self, thread: ThreadRef) {
        // If already on this list, skip -- mirrors do_add_to_requeue's
        // find()+insert() guard in syscall/sync.rs; protected by the
        // caller's sleep_info lock, so no TOCTOU race.
        if !self.threads.find(&thread.objid()).is_null() {
            return;
        }
        self.threads.insert(thread);
    }

    pub fn remove_thread(&mut self, id: ObjID) {
        let mut cursor = self.threads.find_mut(&id);
        cursor.remove();
    }

    /// Claim up to `max_count` waiters into `batch`, stopping early if it fills. Returns whether
    /// the batch filled, i.e. whether there may be more to claim on another pass.
    ///
    /// Claiming is `reset_sync_sleep` plus the removal, both under the caller's `sleep_info` lock;
    /// scheduling the claimed threads is [Object::wakeup_word]'s job, once that lock is gone.
    fn claim_n(&mut self, max_count: usize, batch: &mut WakeBatch) -> bool {
        let mut cursor = self.threads.front_mut();
        while !batch.is_full() && batch.len() < max_count && !cursor.is_null() {
            let thread = cursor.get().unwrap();
            if thread.reset_sync_sleep() {
                let thread = cursor.remove().unwrap();
                // Safety: not full, checked above.
                unsafe { batch.push_unchecked(thread) };
            } else {
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

    pub fn insert(&mut self, offset: usize, thread: ThreadRef) {
        if let Some(se) = self.word(offset) {
            se.add_thread(thread);
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
    }

    pub fn remove(&mut self, offset: usize, thread_id: ObjID) {
        if let Some(se) = self.word(offset) {
            se.remove_thread(thread_id);
        }
    }

    fn claim_n(&mut self, offset: usize, max_count: usize, batch: &mut WakeBatch) -> bool {
        if let Some(se) = self.word(offset) {
            se.claim_n(max_count, batch)
        } else {
            false
        }
    }
}

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
    pub fn wakeup_word(&self, offset: usize, count: usize) -> usize {
        // No critical guard here, and the reason is worth stating because the obvious symmetry with
        // `Request::signal` and `interrupt.rs` is wrong at this site: `sleep_info` is a *sleeping*
        // `Mutex`, not a `Spinlock`, and `Mutex::lock` panics outright in a critical context. A
        // guard around this drain therefore dies on the first thread exit
        // (`wakeup_word` <- `set_state_and_code` <- `Thread::exit`), deterministically.
        //
        // The premise behind wanting one does not hold either. A sleeping mutex masks no
        // interrupts and prevents no preemption, so the drain this replaced was already
        // preemptible; what batching adds is only that a claimed waiter now sits in `batch` on
        // this stack rather than going onto a run queue adjacently. That window is real -- a
        // preemption that exits this thread mid-drain loses those wakeups with their sleep flags
        // already consumed -- but it is exactly the exposure `requeue_all` has always carried,
        // claiming into its own batch with no guard on the same path. Closing it here alone would
        // buy nothing while `requeue_all` is reached from every one of these callers.
        //
        // The fix if it ever needs closing, for both: claim straight onto the requeue list under
        // the lock rather than onto the stack, and leave only `requeue_all` outside. That trades a
        // requeue-list insert and removal onto every wake, so it wants measuring rather than
        // assuming.
        let mut woken = 0;
        while woken < count {
            let mut batch = WakeBatch::new();
            let maybe_more = {
                let mut sleep_info = self.sleep_info.lock();
                sleep_info.claim_n(offset, count - woken, &mut batch)
            };
            if batch.is_empty() {
                break;
            }
            woken += batch.len();
            for thread in batch {
                add_to_requeue(thread);
            }
            // A short batch means the walk reached the end of the sleep entry or ran out of
            // `count`; only a full one says there may be more waiting.
            if !maybe_more {
                break;
            }
        }
        woken
    }

    pub fn add_device_interrupt(&self, vector: u32, num: usize, offset: usize) {
        self.device_interrupt_info[num]
            .0
            .store(vector as u64, Ordering::Release);
        self.device_interrupt_info[num]
            .1
            .store(offset as u64, Ordering::Release);
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
                for i in 0..NUM_DEVICE_INTERRUPTS {
                    let di_offset = self.device_interrupt_info[i].1.load(Ordering::Acquire);
                    let di_vector = self.device_interrupt_info[i].0.load(Ordering::Acquire);
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

        let mut sleep_info = self.sleep_info.lock();
        let cur = vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_64(offset))?;
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
            sleep_info.insert(offset, thread.clone());
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
        let mut sleep_info = self.sleep_info.lock();

        let cur = vaddr
            .map(|vaddr| Ok(vaddr.load(Ordering::SeqCst)))
            .unwrap_or_else(|| self.read_atomic_32(offset))?;
        let res = op.check(cur, val, flags);
        if res {
            if first_sleep {
                thread.set_sync_sleep();
            }
            sleep_info.insert(offset, thread.clone());
        }
        Ok(res)
    }

    pub fn remove_from_sleep_word(&self, offset: usize) {
        let thread = current_thread_ref().unwrap();
        let mut sleep_info = self.sleep_info.lock();
        sleep_info.remove(offset, thread.objid());

        // TODO: I think this only works if the thread waits on one interrupt.
        if self.flags.load(Ordering::Acquire) & OBJ_HAS_INTERRUPTS != 0 {
            for i in 0..NUM_DEVICE_INTERRUPTS {
                let di_offset = self.device_interrupt_info[i].1.load(Ordering::Acquire);
                let di_vector = self.device_interrupt_info[i].0.load(Ordering::Acquire);
                if di_offset as usize == offset {
                    remove_from_device_wait(thread, di_vector as u32);
                    break;
                }
            }
        }
    }
}
