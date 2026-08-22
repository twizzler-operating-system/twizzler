use core::{
    sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering},
    time::Duration,
};

use intrusive_collections::{KeyAdapter, RBTree, intrusive_adapter};
use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSync, ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake, TimeSpan},
    thread::ExecutionState,
    trace::{MAX_BLOCK_NAME, ThreadBlocked, ThreadResumed, TraceEntryFlags, TraceKind},
};
use twizzler_rt_abi::{
    Result,
    error::{ArgumentError, GenericError, TwzError},
};

use crate::{
    instant::Instant,
    memory::{
        VirtAddr,
        context::{
            kernel_context,
            virtmem::{RESOLVE_CHUNK, Slot},
        },
    },
    obj::{LookupFlags, ObjectRef},
    once::Once,
    processor::sched::{SchedFlags, schedule},
    spinlock::Spinlock,
    thread::{CriticalGuard, Thread, ThreadRef, current_memory_context, current_thread_ref},
    trace::{
        mgr::{TRACE_MGR, TraceEvent},
        new_trace_entry,
    },
};

pub struct Requeue {
    list: Spinlock<RBTree<RequeueLinkAdapter>>,
    /// Entries on `list`, readable without taking it.
    ///
    /// Exists for [requeue_all]'s early-out. That runs from every futex wake, every device
    /// interrupt, `set_state_and_code` and the timeout callback, and took this global spinlock at
    /// least once per call *even when the list was empty* -- which is the overwhelmingly common
    /// case. One cache line, serializing every wake in the system, to discover there was nothing
    /// to do.
    ///
    /// **Ordering.** Maintained strictly under `list`'s lock, so it is exact with respect to any
    /// holder of that lock; only the early-out reads it unlocked. A racing reader can see it
    /// stale-low, and that is harmless for a reason the *callers* supply rather than a memory
    /// ordering one: every site that inserts (`add_to_requeue`, `add_all_to_requeue`) calls
    /// `requeue_all` immediately afterwards, and an inserter trivially observes its own increment,
    /// so no entry is left undrained by the pass that put it there. The hardtick backstop in
    /// `oneshot_clock_hardtick` remains as the second line of defence it always was.
    ///
    /// **Bias.** Discrepancies are upward, as with `Object::sleepers`: a thread skipped for being
    /// critical stays counted and costs a later pass a lock it would have taken anyway. The
    /// opposite error -- reading zero with a thread still queued -- is a lost wakeup.
    count: AtomicUsize,
}

impl Requeue {
    /// Was `self.list.lock().iter().count()` -- an O(n) walk under the global lock, for a
    /// diagnostic.
    pub fn len(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

intrusive_adapter!(pub RequeueLinkAdapter = ThreadRef: Thread { requeue_link: intrusive_collections::rbtree::AtomicLink });

impl<'a> KeyAdapter<'a> for RequeueLinkAdapter {
    type Key = ObjID;
    fn get_key(&self, s: &'a Thread) -> ObjID {
        s.objid()
    }
}

/* TODO: make this thread-local */
static REQUEUE: Once<Requeue> = Once::new();

fn get_requeue_list() -> &'static Requeue {
    REQUEUE.call_once(|| Requeue {
        list: Spinlock::new(RBTree::new(RequeueLinkAdapter::NEW)),
        count: AtomicUsize::new(0),
    })
}

/// Threads claimed per pass of [requeue_all].
const REQUEUE_BATCH: usize = 8;

/// Wake everything on the requeue list that can be woken.
///
/// Claim under the lock, schedule outside it. `schedule_thread` is not a small call -- a topology
/// walk in `select_cpu`, a remote run queue lock, and a wakeup IPI -- and running it per entry
/// under this spinlock put all of that, once per waiter, in an interrupts-off region with no bound
/// but the list length.
///
/// Correctness of deferring it: claiming means winning THREAD_IS_SYNC_SLEEP_DONE *and* taking the
/// entry, and every other path to scheduling a thread (`add_to_requeue`'s fast path,
/// `claim_own_wakeup`) must win that same flag first. So a claimed thread is ours alone until we
/// schedule it, and holding it in `batch` for a few instructions is not a window anyone else can
/// use.
///
/// Deferring is also what keeps the drop off the lock. Both `schedule_thread` and
/// `schedule_thread_on_cpu` return early for an exiting thread without storing the reference, so
/// the last one dies at the call -- `Thread::drop` -> `IdCounter::release` -> a sleeping mutex,
/// under a spinlock, which is the wedge `remove_from_requeue` documents.
///
/// Critical across the handoff, though, and that part is not optional. A claimed thread is on no
/// list at all until `schedule_thread` runs -- the flag is consumed and the entry is gone -- so
/// this thread exiting at a poll point in that window (an interrupt return ->
/// `schedule_maybe_preempt` -> `schedule(REINSERT)` -> `maybe_exit` -> `exit`, which never
/// returns) takes the batch and its wakeups with it, unrecoverably: the hardtick backstop drains
/// the requeue *list*, and these are not on it. Before batching the guard came for free -- the
/// walk and `schedule_thread` both ran under `requeue.list.lock()`, and `Spinlock::lock` masks
/// interrupts for the guard's lifetime, so no interrupt return could land mid-drain. Same guard,
/// same reason, as the device-interrupt drain in `interrupt.rs` and the handoff in
/// `Mutex::release`.
pub fn requeue_all() {
    let requeue = get_requeue_list();
    // Nothing queued, so nothing to drain -- and finding that out used to cost this global lock,
    // on every wake in the system. See the ordering and bias notes on `Requeue::count`: a reader
    // can only be stale *low*, and every inserter drains after its own insert.
    if requeue.count.load(Ordering::SeqCst) == 0 {
        return;
    }
    loop {
        let mut batch = heapless::Vec::<ThreadRef, REQUEUE_BATCH>::new();
        // Declared after `batch` so it drops first: see the loop body below.
        let _critical = {
            let mut list = requeue.list.lock();
            let mut cursor = list.front_mut();
            while !batch.is_full() && !cursor.is_null() {
                if cursor
                    .get()
                    .is_some_and(|v| !v.is_critical() && v.reset_sync_sleep_done())
                {
                    if let Some(t) = cursor.remove() {
                        assert!(!t.get_mutex_wait());
                        // Safety: not full, checked above.
                        unsafe { batch.push_unchecked(t) };
                    }
                } else {
                    cursor.move_next();
                }
            }
            // Under the same lock as the claim, so the count can never describe a list this
            // thread has already emptied.
            requeue.count.fetch_sub(batch.len(), Ordering::SeqCst);
            // Taken while the lock still covers the claim, so there is no instant in which a
            // claimed thread is exposed to this thread's own exit.
            current_thread_ref().map(|ct| ct.enter_critical())
        };
        // A short batch means the walk above reached the end of the list, so there is nothing left
        // to claim. A full one says only that we ran out of room; go back for the rest. Entries
        // skipped for being critical or already claimed stay at the front and are re-examined,
        // which is why this terminates on the empty batch rather than on a fixed number of passes.
        let full = batch.is_full();
        if batch.is_empty() {
            return;
        }
        // Cloned rather than moved, so `batch` outlives `_critical` and no reference can reach zero
        // inside the guard: `schedule_thread` drops the one it is given for an exiting thread, and
        // that last drop reaches `IdCounter::release`, which takes a sleeping mutex -- and
        // `Mutex::lock` panics outright in a critical context.
        for t in &batch {
            crate::processor::sched::schedule_thread(t.clone());
        }
        if !full {
            return;
        }
    }
}

/// Insert `thread`, handing the reference **back** when nothing was inserted.
///
/// Returning it rather than dropping it here is the whole point of the signature. The caller holds
/// a spinlock, which makes the current thread critical, and this can hold the last reference to
/// `thread` -- `Thread::drop` reaches `IdCounter::release` and `SecCtxMgr::drop`, both of which
/// take *sleeping* mutexes, and `Mutex::lock` panics outright in a critical context. That is the
/// rule `remove_from_requeue` and `CondVar::signal` already follow.
///
/// `None` also means "inserted", which is what keeps [`Requeue::count`] in step: a duplicate must
/// not be counted, or the count never returns to zero and the early-out in [`requeue_all`] is dead.
#[must_use]
fn do_add_to_requeue(
    list: &mut RBTree<RequeueLinkAdapter>,
    thread: ThreadRef,
) -> Option<ThreadRef> {
    // If already on the list, skip. This can happen with spurious wakeups.
    // The find() + insert() is protected by the caller's lock, so no TOCTOU race.
    if !list.find(&thread.objid()).is_null() {
        return Some(thread);
    }
    list.insert(thread);
    None
}

#[track_caller]
pub fn add_to_requeue(thread: ThreadRef) {
    if !thread.is_critical() && thread.reset_sync_sleep_done() {
        log::trace!(
            "adding {} ({}) to immediate schedule, from {}",
            thread.id(),
            thread.objid(),
            core::panic::Location::caller(),
        );
        let id = thread.objid();
        assert!(!thread.get_mutex_wait());
        crate::processor::sched::schedule_thread(thread);
        let requeue = get_requeue_list();
        // See `remove_from_requeue`: the removed reference must not be dropped under the spinlock.
        // `schedule_thread` above returns early for an exiting thread without storing the one it
        // was given, so a stale entry here can hold the last reference, and `Thread::drop` ->
        // `IdCounter::release` takes a sleeping mutex.
        let removed = {
            let mut list = requeue.list.lock();
            let removed = list.find_mut(&id).remove();
            if removed.is_some() {
                requeue.count.fetch_sub(1, Ordering::SeqCst);
            }
            removed
        };
        drop(removed);
        return;
    }
    log::trace!(
        "adding {} ({}) to requeue, from {}",
        thread.id(),
        thread.objid(),
        core::panic::Location::caller()
    );
    let requeue = get_requeue_list();
    // Dropped outside the lock; see [`do_add_to_requeue`].
    let leftover = {
        let mut list = requeue.list.lock();
        let leftover = do_add_to_requeue(&mut *list, thread);
        if leftover.is_none() {
            requeue.count.fetch_add(1, Ordering::SeqCst);
        }
        leftover
    };
    drop(leftover);
}

pub fn add_all_to_requeue(iter: impl IntoIterator<Item = ThreadRef>) {
    let requeue = get_requeue_list();
    // We are going to try to enqueue all threads. Best case, we can just immediately
    // schedule the thread, but if not, enqueue it onto the requeue list for later.
    //
    // In the best-best case scenario, we don't even need to take the requeue lock -- which is
    // still true, because the fast branch below never takes it.
    //
    // The lock is taken and released **per thread**, rather than once and held across the loop.
    // Holding it made every later iteration critical, and both things a later iteration can do
    // release a `ThreadRef`: `schedule_thread` returns early for an exiting thread without
    // storing the one it was given (see `add_to_requeue`), and `do_add_to_requeue` hands back the
    // reference for one already queued. Either can be the last, and `Thread::drop` takes sleeping
    // mutexes (`IdCounter::release`, `SecCtxMgr::drop`) that `Mutex::lock` refuses in a critical
    // context. That is a panic, not a slow path, and it is the one in `ocdperf.md` -- reached
    // from `MemoryTracker::wake` under `DeferredUnmappingOps::run_all`.
    //
    // The extra acquisitions land only on threads that could not be scheduled outright, which
    // this function already treats as the uncommon case.
    for thread in iter.into_iter() {
        if !thread.is_critical() && thread.reset_sync_sleep_done() {
            assert!(!thread.get_mutex_wait());
            crate::processor::sched::schedule_thread(thread);
        } else {
            let leftover = {
                let mut list = requeue.list.lock();
                let leftover = do_add_to_requeue(&mut *list, thread);
                if leftover.is_none() {
                    requeue.count.fetch_add(1, Ordering::SeqCst);
                }
                leftover
            };
            drop(leftover);
        }
    }
}

/// Drop any pending requeue entry for `thread` without acting on it. Cleanup only -- use
/// [claim_own_wakeup] if the caller is about to decide whether to block.
pub fn remove_from_requeue(thread: &ThreadRef) {
    let requeue = get_requeue_list();
    // Drop the removed reference outside the spinlock. It can be the last one, and `Thread::drop`
    // returns its id through `IdCounter::release`, which takes a sleeping mutex -- a mutex under a
    // spinlock, which wedges every cpu that later wants the requeue lock.
    let removed = {
        let mut list = requeue.list.lock();
        let removed = list.find_mut(&thread.objid()).remove();
        if removed.is_some() {
            requeue.count.fetch_sub(1, Ordering::SeqCst);
        }
        if removed.is_some() {
            requeue.count.fetch_sub(1, Ordering::SeqCst);
        }
        removed
    };
    drop(removed);
}

/// Take a wakeup a waker already parked on the requeue list for `thread`, returning true if there
/// was one. The caller must then *not* block: the waker has already done its half.
///
/// This exists because `requeue_all()` cannot rescue the calling thread -- it skips anything that
/// is_critical(), and a thread runs this while holding its own critical guard. Consuming
/// THREAD_IS_SYNC_SLEEP_DONE here is what keeps that invariant intact: every way off the requeue
/// list (here, `requeue_all`, and `add_to_requeue`'s fast path) claims the flag exactly once, so a
/// wakeup can never be counted twice or left half-applied.
///
/// True requires *winning the flag*, not merely finding an entry, because a caller acts on this by
/// not blocking -- and an entry alone does not mean nobody has already made it runnable.
/// `add_all_to_requeue`'s fast path calls `schedule_thread` without removing any entry the thread
/// already had, so a stale entry can sit on the list while a waker that won the flag has put the
/// thread on a run queue. Removing that entry proves nothing. Winning the flag does: every path
/// that schedules a thread must take it first, so taking it ourselves means no one else did.
///
/// The claim below holds the requeue lock across both halves, which settles it against everything
/// that takes that lock. It cannot settle it against the fast paths in `add_to_requeue` and
/// `add_all_to_requeue`, which test the flag with no lock at all -- so the result is still a race
/// to be checked, not an outcome to be assumed.
///
/// A caller that is critical (the one below `set_sync_sleep_done`) cannot observe the difference:
/// both the fast path and `requeue_all` skip a critical thread, so an entry there always comes with
/// the flag still set.
pub fn claim_own_wakeup(thread: &ThreadRef) -> bool {
    let requeue = get_requeue_list();
    let (removed, claimed) = {
        let mut list = requeue.list.lock();
        let removed = list.find_mut(&thread.objid()).remove();
        if removed.is_some() {
            requeue.count.fetch_sub(1, Ordering::SeqCst);
        }
        // Both under the lock, so no other holder of it can catch the half-state where the entry is
        // gone but the flag is still set: against `requeue_all` and the two slow paths, taking the
        // entry and taking the flag are one step. Order still matters within it -- the flag is
        // taken only once the entry is ours, so losing leaves the wakeup with whoever won rather
        // than eating it here.
        let claimed = removed.is_some() && thread.reset_sync_sleep_done();
        (removed, claimed)
    };
    // See remove_from_requeue: the removed reference must not be dropped under the spinlock.
    drop(removed);
    claimed
}

/// Whether this thread must return from a thread-sync sleep instead of entering one.
///
/// A force-exit sets THREAD_MUST_EXIT and expects the target to notice it at its next poll point,
/// but a thread that blocks here has no next poll point: `finish_blocking` reaches `schedule`
/// without SchedFlags::REINSERT, which is the only arm that calls `maybe_exit`, and it will not
/// cross the kernel boundary again until something wakes it. If the word it is sleeping on belongs
/// to a peer that is exiting too, nothing ever does.
///
/// Checked under the caller's critical guard, after set_sync_sleep_done, so it catches every
/// force-exit that lands before we commit to blocking -- which is the case that occurs, since
/// `ChangeState` only reaches a thread the monitor believes is still running. A force-exit against
/// a thread that is *already* parked here is not covered: `force_exit` does not wake its target,
/// and waking one from outside is not a flag poke (see the note there).
///
/// The caller treats a true here exactly like a claimed wakeup -- drop the guard, do not block --
/// and its normal post-sleep cleanup (undo_sleep, remove_from_requeue, resetting the sleep flags)
/// runs either way. The exit itself happens in `sys_thread_sync`, once that cleanup is done.
///
/// A force-exit restricted to a security context (see `sys_thread_change_state_in_sctx`) is not one
/// this thread can act on yet, so it must still be allowed to block: refusing would spin it against
/// whatever it is waiting for until it happens to return to its own compartment.
fn must_not_block(thread: &ThreadRef) -> bool {
    thread.exit_deliverable()
}

/// How long after a force-exit to wake the target, for both the registration below and
/// [`Thread::force_exit`]'s.
///
/// A tick rather than zero, and the delay is load-bearing rather than a courtesy to a thread that
/// is about to die anyway. A 0ns entry lands in the current window, so it can fire from the timeout
/// thread while the target is still critical between registering it and reaching `schedule` --
/// where `add_to_requeue` can only park it on the requeue list, and nothing collects that before it
/// sleeps. Firing a tick later means the target is parked in the ordinary way first, and the
/// callback takes the ordinary wake path.
const FORCE_EXIT_WAKE_NS: u64 = 1_000_000;

/// Number of timeout windows to spread force-exit wakes across.
///
/// A window holds `NR_WINDOW_ENTRIES` (32) entries, and past that `TimeoutQueue::insert` runs the
/// callback inline under its own lock -- which is the racy immediate claim the delay above exists
/// to avoid. `main_thread_exited` force-exits a compartment's threads in a loop, so without this
/// they all land in the one window a tick from now.
const FORCE_EXIT_WAKE_SPREAD: u64 = 8;

/// Delay before waking `thread` to take a pending force-exit. See [`FORCE_EXIT_WAKE_NS`] for why it
/// is not zero and [`FORCE_EXIT_WAKE_SPREAD`] for why it varies by thread.
pub(crate) fn force_exit_wake_ns(thread: &ThreadRef) -> u64 {
    FORCE_EXIT_WAKE_NS * (1 + thread.id() % FORCE_EXIT_WAKE_SPREAD)
}

/// A clock reading for the `log::trace!` timing breakdowns below, taken only when that log is
/// actually enabled.
///
/// `Instant::now()` is a `Once` poll, an indirect call through the registered tick source and an
/// `rdtsc`, and the compiler cannot see through the virtual call to elide it -- so the readings
/// were taken whether or not anything consumed them. `sys_thread_sync` is the busiest syscall in
/// the system and these paths take up to four apiece, which made an untraced futex operation pay
/// for four clock reads it then threw away.
///
/// The zero fallback only ever reaches a `log::trace!` that is not being emitted;
/// `checked_sub_instant` yields `Duration::ZERO` against it rather than a wrong number.
#[inline]
fn trace_now() -> Instant {
    if log::log_enabled!(log::Level::Trace) {
        Instant::now()
    } else {
        Instant::zero()
    }
}

pub fn trace_block(_th: &ThreadRef, name: impl AsRef<str>) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_BLOCK) {
        let name = name.as_ref();
        let mut block_name = [0; MAX_BLOCK_NAME];
        let len = name.as_bytes().len().min(MAX_BLOCK_NAME);
        (&mut block_name[0..len]).copy_from_slice(&name.as_bytes()[0..len]);
        let block = ThreadBlocked {
            block_name,
            block_name_len: len as u32,
        };
        let entry = new_trace_entry(
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_BLOCK,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, block));
    }
}

pub fn trace_resume(_th: &ThreadRef, duration: TimeSpan) {
    if TRACE_MGR.any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_RESUME) {
        let data = ThreadResumed { duration };
        let entry = new_trace_entry(
            TraceKind::Thread,
            twizzler_abi::trace::THREAD_RESUME,
            TraceEntryFlags::HAS_DATA,
        );
        TRACE_MGR.async_enqueue(TraceEvent::new_with_data(entry, data));
    }
}
// TODO: this is gross, we're manually trading out a critical guard with an interrupt guard because
// we don't want to get interrupted... we need a better way to do this kind of consumable "don't
// schedule until I say so".
pub fn finish_blocking(guard: CriticalGuard) {
    let thread = current_thread_ref().unwrap();
    // Only `trace_resume` consumes this, and it is gated on a sink existing. Reading the clock
    // unconditionally charged every block -- the one path where a wasted indirect call is on the
    // critical latency, not beside it.
    let start = TRACE_MGR
        .any_enabled(TraceKind::Thread, twizzler_abi::trace::THREAD_RESUME)
        .then(Instant::now);
    trace_block(&thread, "thread-sync");
    crate::interrupt::with_disabled(|| {
        if must_not_block(thread) {
            let _timeout_key = crate::clock::register_timeout_callback(
                force_exit_wake_ns(thread),
                thread_sync_cb_timeout,
                thread.clone(),
                thread.sync_sleep_gen(),
            );
        }
        drop(guard);
        // No claim_own_wakeup here, deliberately. A wakeup parked on the requeue list while we held
        // the guard is real and blocking on it does cost a lost wake, but this is the wrong place
        // to take it: every caller owns a different wait-list link, and returning without blocking
        // means unlinking it. `Request::setup_wait` unlinks `pager_link` when it claims -- staying
        // linked is what makes the *next* setup_wait panic with "already linked" -- and the memory
        // tracker unlinks `memwait_link` in the same situation, for the same reason. By the time we
        // get here those callers have handed us the guard and cannot do either. So the claim stays
        // where the link is known: sys_thread_sync, setup_wait, and CondVar::wait each do their own
        // just before calling us.
        thread.set_state(ExecutionState::Sleeping);
        schedule(SchedFlags::YIELD | SchedFlags::PREEMPT);
        thread.set_state(ExecutionState::Running);
        assert!(!thread.mutex_link.is_linked());
    });
    // A sink that appeared while we were blocked still gets its event, with a zero duration --
    // same treatment the syscall-exit trace gives a call nothing timed.
    let duration = start
        .map(|start| (Instant::now() - start).into())
        .unwrap_or_default();
    trace_resume(&thread, duration);
}

// TODO: uses-virtaddr
fn get_obj_and_offset(addr: VirtAddr) -> Result<(ObjectRef, usize, Option<*const u8>)> {
    // let t = current_thread_ref().unwrap();
    // TODO: prevent user from waiting on kernel object memory
    let user_vmc = current_memory_context();
    let vmc = user_vmc
        .as_ref()
        .map(|x| &**x)
        .unwrap_or_else(|| &kernel_context());
    let object = vmc
        .lookup_object_ref_cached(addr.try_into().map_err(|_| ArgumentError::InvalidAddress)?)
        .ok_or(ArgumentError::InvalidAddress)?;
    let offset = (addr.raw() as usize) % (1024 * 1024 * 1024); //TODO: arch-dep, centralize these calculations somewhere, see PageNumber
    Ok((object, offset, Some(addr.as_ptr())))
}

fn get_obj(reference: ThreadSyncReference) -> Result<(ObjectRef, usize, Option<*const u8>)> {
    Ok(match reference {
        ThreadSyncReference::ObjectRef(id, offset) => {
            let obj = match crate::obj::lookup_object(id, LookupFlags::empty()) {
                crate::obj::LookupResult::Found(o) => o,
                _ => return Err(ArgumentError::InvalidAddress.into()),
            };
            (obj, offset, None)
        }
        // Not `unwrap`: the address comes straight from userspace, and `VirtAddr::new` rejects a
        // non-canonical one -- which made a bad pointer here a kernel panic rather than an `EINVAL`
        // for the op. The batched path in `resolve_ops` cannot panic on this by construction, so
        // this also keeps the two agreeing.
        ThreadSyncReference::Virtual(addr) => get_obj_and_offset(
            VirtAddr::new(addr as u64).map_err(|_| ArgumentError::InvalidAddress)?,
        )?,
        ThreadSyncReference::Virtual32(addr) => get_obj_and_offset(
            VirtAddr::new(addr as u64).map_err(|_| ArgumentError::InvalidAddress)?,
        )?,
    })
}

struct SleepEvent {
    obj: ObjectRef,
    offset: usize,
    did_sleep: bool,
}

type Resolved = Result<(ObjectRef, usize, Option<*const u8>)>;

fn prep_sleep(sleep: &ThreadSyncSleep, first_sleep: bool) -> Result<SleepEvent> {
    prep_sleep_with(sleep, first_sleep, get_obj(sleep.reference))
}

fn prep_sleep_with(
    sleep: &ThreadSyncSleep,
    first_sleep: bool,
    resolved: Resolved,
) -> Result<SleepEvent> {
    let (obj, offset, vaddr) = resolved?;
    if first_sleep {
        if let Some(thread) = current_thread_ref() {
            thread.note_sleep_word(obj.id(), offset);
        }
    }

    let did_sleep = if matches!(sleep.reference, ThreadSyncReference::Virtual32(_)) {
        let vaddr = vaddr
            .map(|vaddr| unsafe { vaddr.cast::<AtomicU32>().as_ref() })
            .flatten();
        obj.setup_sleep_word32(
            offset,
            sleep.op,
            sleep.value as u32,
            first_sleep,
            sleep.flags,
            vaddr,
        )
    } else {
        let vaddr = vaddr
            .map(|vaddr| unsafe { vaddr.cast::<AtomicU64>().as_ref() })
            .flatten();
        obj.setup_sleep_word(
            offset,
            sleep.op,
            sleep.value,
            first_sleep,
            sleep.flags,
            vaddr,
        )
    }?;

    Ok(SleepEvent {
        obj,
        offset,
        did_sleep,
    })
}

fn undo_sleep(sleep: &SleepEvent) {
    sleep.obj.remove_from_sleep_word(sleep.offset);
}

pub fn wakeup(wake: &ThreadSyncWake) -> Result<usize> {
    wakeup_with(wake, get_obj(wake.reference))
}

fn wakeup_with(wake: &ThreadSyncWake, resolved: Resolved) -> Result<usize> {
    let (obj, offset, _) = resolved?;
    Ok(obj.wakeup_word(offset, wake.count))
}

/// Resolve a chunk of ops' references together, so the virtual ones share one `regions`
/// acquisition instead of taking it apiece.
///
/// Pure: it reads the slot -> object mapping and nothing else, which is what makes hoisting it out
/// of `prep_sleep`/`wakeup` safe. Errors stay per-op -- a reference that fails to resolve yields
/// `Err` in its own slot and the caller writes it to that op's result exactly as before, since a
/// failed `get_obj` was already non-fatal to the rest of the call.
///
/// One behavioural difference, stated because it is the only one: a `Wake` earlier in the array
/// can no longer influence what a later op resolves to. On smp a woken thread could remap a slot
/// between the two, and previously the later op would have seen the new mapping. Nothing ordered
/// the mapping against the use before either -- this widens an existing window rather than opening
/// a new kind.
fn resolve_ops(ops: &[ThreadSync]) -> heapless::Vec<Resolved, RESOLVE_CHUNK> {
    let user_vmc = current_memory_context();
    let vmc = user_vmc
        .as_ref()
        .map(|x| &**x)
        .unwrap_or_else(|| &kernel_context());

    let reference_of = |op: &ThreadSync| match op {
        ThreadSync::Sleep(sleep, _) => sleep.reference,
        ThreadSync::Wake(wake, _) => wake.reference,
    };

    // Dense list of the virtual references, plus which op each came from.
    let mut slots = heapless::Vec::<Slot, RESOLVE_CHUNK>::new();
    let mut owners = heapless::Vec::<(usize, u64), RESOLVE_CHUNK>::new();
    for (i, op) in ops.iter().enumerate() {
        let addr = match reference_of(op) {
            ThreadSyncReference::Virtual(addr) => addr as u64,
            ThreadSyncReference::Virtual32(addr) => addr as u64,
            ThreadSyncReference::ObjectRef(..) => continue,
        };
        let Some(slot) = VirtAddr::new(addr)
            .ok()
            .and_then(|va| TryInto::<Slot>::try_into(va).ok())
        else {
            continue;
        };
        let _ = slots.push(slot);
        let _ = owners.push((i, addr));
    }

    let mut objs = [const { None }; RESOLVE_CHUNK];
    if !slots.is_empty() {
        vmc.lookup_object_refs_cached(&slots, &mut objs[..slots.len()]);
    }

    let mut out = heapless::Vec::<Resolved, RESOLVE_CHUNK>::new();
    for (i, op) in ops.iter().enumerate() {
        let resolved = match reference_of(op) {
            ThreadSyncReference::ObjectRef(id, offset) => {
                match crate::obj::lookup_object(id, LookupFlags::empty()) {
                    crate::obj::LookupResult::Found(o) => Ok((o, offset, None)),
                    _ => Err(ArgumentError::InvalidAddress.into()),
                }
            }
            _ => match owners.iter().position(|(owner, _)| *owner == i) {
                Some(dense) => match objs[dense].take() {
                    // Same offset arithmetic as `get_obj_and_offset`: the modulus is the slot size,
                    // so the region is never consulted for it.
                    Some(obj) => Ok((
                        obj,
                        (owners[dense].1 as usize) % (1024 * 1024 * 1024),
                        Some(owners[dense].1 as *const u8),
                    )),
                    None => Err(ArgumentError::InvalidAddress.into()),
                },
                None => Err(ArgumentError::InvalidAddress.into()),
            },
        };
        let _ = out.push(resolved);
    }
    out
}

pub(crate) fn thread_sync_cb_timeout(thread: ThreadRef, sleep_gen: u64) {
    // The sleep we were registered for is over, so this timeout has no one to wake. Bail before
    // touching the sleep flags: they belong to whatever the thread is doing now, and consuming them
    // here is what used to schedule an already-running thread. Reached whenever the thread woke for
    // another reason after `soft_advance` dequeued us, which is past the point `release` can help.
    if thread.sync_sleep_gen() != sleep_gen {
        return;
    }
    if thread.reset_sync_sleep() {
        add_to_requeue(thread);
    }
    requeue_all();
}

fn simple_timed_sleep(timeout: &&mut Duration) {
    let thread = current_thread_ref().unwrap();
    thread.set_sync_sleep();
    let timeout_key = crate::clock::register_timeout_callback(
        // TODO: fix all our time types
        timeout.as_nanos() as u64,
        thread_sync_cb_timeout,
        thread.clone(),
        thread.sync_sleep_gen(),
    );
    let guard = thread.enter_critical();
    thread.set_sync_sleep_done();
    requeue_all();
    // requeue_all() above cannot rescue *us*: it skips any thread that is_critical(), and we
    // hold `guard`. So check for ourselves -- if a waker parked us on the requeue list before
    // set_sync_sleep_done() above, that wakeup has already happened and blocking on it now
    // would mean sleeping until some unrelated requeue_all() came along.
    if claim_own_wakeup(&thread) {
        drop(guard);
    } else {
        finish_blocking(guard);
    }
    let _guard = thread.enter_critical();
    // Before anything else, and before releasing the key: retiring the token is the only thing that
    // stops a callback already past `soft_advance`, and it has to happen while the sleep flags are
    // still ours to protect.
    thread.end_sync_sleep();
    remove_from_requeue(&thread);
    timeout_key.release();
    thread.reset_sync_sleep();
    thread.reset_sync_sleep_done();
}

pub fn optimized_single_sleep(op: ThreadSyncSleep) -> Result<bool> {
    let start = trace_now();
    let se = prep_sleep(&op, true)?;

    if !se.did_sleep {
        return Ok(false);
    }
    let thread = current_thread_ref().unwrap();
    let guard = thread.enter_critical();
    thread.set_sync_sleep_done();
    requeue_all();
    let prep_done = trace_now();
    // See simple_timed_sleep: requeue_all() skips critical threads, so it can never rescue the
    // caller. Take a wakeup that raced in ahead of set_sync_sleep_done() ourselves.
    if claim_own_wakeup(&thread) {
        drop(guard);
    } else {
        finish_blocking(guard);
    }
    let woke_up = trace_now();
    let _guard = thread.enter_critical();
    thread.reset_sync_sleep();
    thread.reset_sync_sleep_done();
    remove_from_requeue(&thread);
    drop(_guard);
    undo_sleep(&se);
    // If we have a timeout key, AND we don't find it during release, the timeout fired.
    let done = trace_now();
    log::trace!(
        "{}: ts-optimized-sleep: {:7?} {:7?} {:7?}",
        current_thread_ref().unwrap().id(),
        prep_done - start,
        woke_up - prep_done,
        done - woke_up
    );

    Ok(true)
}

pub fn optimized_single_wake(op: ThreadSyncWake) -> Result<usize> {
    let start = trace_now();
    let count = wakeup(&op)?;
    requeue_all();
    let done = trace_now();
    log::trace!(
        "{}: ts-optimized-wake {}: {:7?}",
        current_thread_ref().unwrap().id(),
        count,
        done - start
    );

    Ok(count)
}

fn do_sys_thread_sync(ops: &mut [ThreadSync], timeout: Option<&mut Duration>) -> Result<usize> {
    if let Some(ref timeout) = timeout {
        log::trace!(
            "{}: simple timed sleep ({} ms)",
            current_thread_ref().unwrap().id(),
            timeout.as_millis()
        );
        if ops.is_empty() {
            simple_timed_sleep(timeout);
            return Ok(0);
        }
    }

    if ops.len() == 1 && timeout.is_none() {
        match &mut ops[0] {
            ThreadSync::Sleep(thread_sync_sleep, res) => {
                log::trace!(
                    "{}: optimized sleep {:?}",
                    current_thread_ref().unwrap().id(),
                    thread_sync_sleep
                );
                let did_sleep = optimized_single_sleep(*thread_sync_sleep);
                *res = did_sleep.map(|_| 0);
                return did_sleep.map(|x| if x { 1 } else { 0 });
            }
            ThreadSync::Wake(thread_sync_wake, res) => {
                log::trace!(
                    "{}: optimized wake {:?}",
                    current_thread_ref().unwrap().id(),
                    thread_sync_wake
                );
                *res = optimized_single_wake(*thread_sync_wake);
                return Ok(1);
            }
        }
    }

    if ops.len() > 1024 {
        return Err(TwzError::INVALID_ARGUMENT);
    }

    let start = trace_now();
    let first = ops[0];

    let mut ready_count = 0;
    let mut unsleeps = heapless::Vec::<_, 1024>::new();
    let mut num_sleepers = 0;

    // Chunked so that each group's virtual references share one `regions` acquisition. The body is
    // otherwise unchanged, including `unsleeps.is_empty()` as the first-sleep test, which stays
    // correct across chunk boundaries because it reads accumulated state rather than position.
    for chunk in ops.chunks_mut(RESOLVE_CHUNK) {
        let mut resolved = resolve_ops(chunk).into_iter();
        for op in chunk.iter_mut() {
            let resolved = resolved
                .next()
                .unwrap_or(Err(ArgumentError::InvalidAddress.into()));
            match op {
                ThreadSync::Sleep(sleep, result) => {
                    match prep_sleep_with(sleep, unsleeps.is_empty(), resolved) {
                        Ok(se) => {
                            num_sleepers += 1;
                            *result = Ok(if se.did_sleep { 0 } else { 1 });
                            if se.did_sleep && !unsleeps.is_full() {
                                unsafe { unsleeps.push_unchecked(se) };
                            } else {
                                ready_count += 1;
                            }
                        }
                        Err(x) => *result = Err(x),
                    }
                }
                ThreadSync::Wake(wake, result) => match wakeup_with(wake, resolved) {
                    Ok(count) => {
                        *result = Ok(count);
                        if count > 0 {
                            ready_count += 1;
                        }
                    }
                    Err(x) => {
                        *result = Err(x);
                    }
                },
            }
        }
    }

    let prep_done = trace_now();
    let thread = current_thread_ref().unwrap();
    assert!(!thread.mutex_link.is_linked());
    let should_sleep = unsleeps.len() == num_sleepers && num_sleepers > 0;
    let (timeout_key, _guard) = {
        let guard = thread.enter_critical();
        let timeout_key = if should_sleep {
            let timeout_key = timeout.map(|timeout| {
                crate::clock::register_timeout_callback(
                    // TODO: fix all our time types
                    timeout.as_nanos() as u64,
                    thread_sync_cb_timeout,
                    thread.clone(),
                    thread.sync_sleep_gen(),
                )
            });
            timeout_key
        } else {
            None
        };
        requeue_all();
        thread.set_sync_sleep_done();
        assert!(!thread.mutex_link.is_linked());
        let guard = if should_sleep {
            // Catch any wake() that raced in and parked us on the requeue list before
            // sync_sleep_done was set above. This has to be a self-check rather than another
            // requeue_all(): that call skips any is_critical() thread, and we hold `guard`.
            // Only on the sleeping path -- if we aren't about to block, claiming here would
            // consume a waker's wakeup and throw it away.
            if claim_own_wakeup(&thread) {
                drop(guard);
            } else {
                finish_blocking(guard);
            }
            thread.enter_critical()
        } else {
            if thread.reset_sync_sleep() {
                add_to_requeue(thread.clone());
            }
            requeue_all();
            if unsleeps.len() > 0 {
                finish_blocking(guard);
            } else {
                drop(guard);
            }

            thread.enter_critical()
        };
        (timeout_key, guard)
    };

    let woke_up = trace_now();
    // See simple_timed_sleep: retire any outstanding timeout callback before touching the flags it
    // would otherwise consume.
    thread.end_sync_sleep();
    thread.reset_sync_sleep();
    thread.reset_sync_sleep_done();
    drop(_guard);
    for op in &unsleeps {
        undo_sleep(op);
    }
    remove_from_requeue(&thread);
    drop(unsleeps);

    // If we have a timeout key, AND we don't find it during release, the timeout fired.
    let was_timedout = timeout_key.map(|tk| !tk.release()).unwrap_or(false);
    let done = trace_now();
    log::trace!(
        "ts[0]: {} {:7?} {:7?} {:7?}",
        match first {
            ThreadSync::Sleep(_thread_sync_sleep, _) => "sleep",
            ThreadSync::Wake(_thread_sync_wake, _) => " wake",
        },
        prep_done - start,
        woke_up - prep_done,
        done - woke_up
    );
    if was_timedout && ready_count == 0 {
        Err(GenericError::TimedOut.into())
    } else {
        Ok(ready_count)
    }
}

/// Whether this sleep's condition already fails, i.e. the caller would not block on it. `None`
/// when the word is not reachable as a plain user address -- the `ObjectRef` case, which has to go
/// through the object.
/// How much a one-pass resolution of a call's references would save.
///
/// Each virtual-referenced op resolves its slot independently, taking `regions` per op. Resolving
/// them together under one acquisition would save `n - 1` of them for a call carrying `n`, so this
/// counts exactly that, rather than a histogram to eyeball. It is an upper bound on the win: it
/// assumes every op would otherwise have taken the lock, which the memo makes untrue for its hits.
///
/// The question it exists to answer is whether the multi-op case is common at all. It may not be:
/// `do_sys_thread_sync` special-cases `ops.len() == 1` with `optimized_single_sleep`/`_wake`, and
/// that path was built deliberately. If calls are overwhelmingly single-op, batching is moot no
/// matter how attractive it looks in the abstract.
pub mod syncbatch {
    use core::sync::atomic::{AtomicU64, Ordering};

    use twizzler_abi::syscall::{ThreadSync, ThreadSyncReference};

    static CALLS: AtomicU64 = AtomicU64::new(0);
    static VIRT_OPS: AtomicU64 = AtomicU64::new(0);
    static SAVEABLE: AtomicU64 = AtomicU64::new(0);
    static MULTI_CALLS: AtomicU64 = AtomicU64::new(0);

    pub fn note_call(ops: &[ThreadSync]) {
        let nvirt = ops
            .iter()
            .filter(|op| {
                let reference = match op {
                    ThreadSync::Sleep(sleep, _) => sleep.reference,
                    ThreadSync::Wake(wake, _) => wake.reference,
                };
                matches!(
                    reference,
                    ThreadSyncReference::Virtual(_) | ThreadSyncReference::Virtual32(_)
                )
            })
            .count() as u64;
        CALLS.fetch_add(1, Ordering::Relaxed);
        VIRT_OPS.fetch_add(nvirt, Ordering::Relaxed);
        SAVEABLE.fetch_add(nvirt.saturating_sub(1), Ordering::Relaxed);
        if nvirt > 1 {
            MULTI_CALLS.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn print() {
        let calls = CALLS.load(Ordering::Relaxed);
        if calls == 0 {
            return;
        }
        let virt_ops = VIRT_OPS.load(Ordering::Relaxed);
        let saveable = SAVEABLE.load(Ordering::Relaxed);
        let multi = MULTI_CALLS.load(Ordering::Relaxed);
        logln!(
            "== sync batching: {} calls ({} with >1 virt op, {}%), {} virt ops, {} of them saveable by one-pass ({}%) ==",
            calls,
            multi,
            calls_pct(multi, calls),
            virt_ops,
            saveable,
            calls_pct(saveable, virt_ops),
        );
    }

    fn calls_pct(part: u64, whole: u64) -> u64 {
        if whole == 0 { 0 } else { part * 100 / whole }
    }
}

fn sleep_word_ready(sleep: &ThreadSyncSleep) -> Option<bool> {
    let addr = match sleep.reference {
        ThreadSyncReference::Virtual(addr) => addr as u64,
        ThreadSyncReference::Virtual32(addr) => addr as u64,
        ThreadSyncReference::ObjectRef(..) => return None,
    };
    // `get_obj` would reject a kernel address by failing to find a user mapping for it; this read
    // runs ahead of that, so it has to do its own rejecting.
    if VirtAddr::new(addr).ok()?.is_kernel() {
        return None;
    }
    Some(match sleep.reference {
        ThreadSyncReference::Virtual32(_) => {
            let cur = unsafe { &*(addr as *const AtomicU32) }.load(Ordering::SeqCst);
            !sleep.op.check(cur, sleep.value as u32, sleep.flags)
        }
        _ => {
            let cur = unsafe { &*(addr as *const AtomicU64) }.load(Ordering::SeqCst);
            !sleep.op.check(cur, sleep.value, sleep.flags)
        }
    })
}

/// Read every virtually-referenced sleep word before the round opens, taking the whole call if one
/// of them already says not to sleep.
///
/// Two jobs. The cheap one: a caller that would not have blocked skips `reserve`, the slot slab
/// and the undo bookkeeping.
///
/// The load-bearing one: these reads happen *before* `reserve`, so a fault on a pager-backed page
/// resolves with no round open. Faulting inside the round re-enters `sys_thread_sync` through the
/// pager's queue wait and trips `reserve`'s mid-round assert. Walking `ops` prefaults the user
/// slice for the same reason -- `create_user_slice` leaves it in place rather than copying it.
///
/// Only an all-sleep array can return early: a `Wake` has to run, and in array order. Writing
/// `*result` here is safe when we do not, because every arm of the main loop reassigns it.
fn ready_before_round(ops: &mut [ThreadSync]) -> Option<usize> {
    let mut ready = 0;
    let mut can_shortcut = true;
    for op in &mut *ops {
        let ThreadSync::Sleep(sleep, result) = op else {
            can_shortcut = false;
            continue;
        };
        match sleep_word_ready(sleep) {
            Some(true) => {
                ready += 1;
                *result = Ok(1);
            }
            Some(false) => *result = Ok(0),
            None => can_shortcut = false,
        }
    }
    (can_shortcut && ready > 0).then_some(ready)
}

pub fn sys_thread_sync(ops: &mut [ThreadSync], timeout: Option<&mut Duration>) -> Result<usize> {
    let thread = current_thread_ref().unwrap();
    super::note_thread_sync_ops(ops);
    syncbatch::note_call(ops);
    // Recursion: a second `sys_thread_sync` entered from inside the first one's round. The way in
    // is a fault on a pager-backed page the outer round touched, which reaches the pager's queue
    // wait -- see `ready_before_round`, which exists to make that rarer.
    //
    // The slot slab is per round and not reentrant, so sleeping again from here would trip
    // `reserve`'s assert. Refuse instead and report zero ready: every caller of a sleep re-checks
    // in a loop (`RawQueue::submit` takes its wait as a closure and calls it from one), so a
    // refusal costs a spin rather than a lost wakeup. A wake needs no slot, and dropping one would
    // strand whoever is waiting on it, so those still run.
    if thread.sync_links.is_linked() {
        if ops.iter().any(|op| matches!(op, ThreadSync::Sleep(..))) {
            if crate::thread::locktrack::diag::NESTED_SYNC_SLEEP.hit() {
                emerglogln!(
                    "thread {} ({}) recursed into sys_thread_sync with a sleep; refusing to sleep",
                    thread.id(),
                    thread.objid(),
                );
            }
            return Ok(0);
        }
        return do_sys_thread_sync(ops, timeout);
    }
    // Ahead of `reserve`, deliberately: see `ready_before_round`. Oversized arrays are the main
    // loop's error to report, so leave them alone.
    if ops.len() <= 1024 {
        if let Some(ready) = ready_before_round(ops) {
            thread.maybe_exit();
            return Ok(ready);
        }
    }
    thread.sync_links.reserve(ops.len(), thread);
    thread.set_timed_wait(timeout.is_some());

    let r = do_sys_thread_sync(ops, timeout);
    if r.is_err() {
        log::trace!(
            "thread {} ({}) failed thread_sync: {:?} ({:?})",
            thread.id(),
            thread.objid(),
            r,
            ops
        );
    }
    thread.sync_links.reset();
    thread.set_timed_wait(false);

    // The one point on this path where exiting is safe: every sleep word has been undone, the
    // requeue entry is gone, the sleep flags are clear, and no guard is held. A thread that got
    // here via `must_not_block` is here precisely to do this; for anyone else it is a no-op.
    // Without it, a force-exit delivered mid-sleep would only be noticed on the next kernel entry,
    // which for a thread whose whole job is to wait may never come.
    thread.maybe_exit();

    r
}
