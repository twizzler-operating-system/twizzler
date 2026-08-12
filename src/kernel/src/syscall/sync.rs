use core::{
    sync::atomic::{AtomicU32, AtomicU64},
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
        context::{UserContext, kernel_context},
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
}

impl Requeue {
    pub fn len(&self) -> usize {
        self.list.lock().iter().count()
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
    })
}

pub fn requeue_all() {
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    let mut cursor = list.front_mut();
    while !cursor.is_null() {
        if cursor
            .get()
            .is_some_and(|v| !v.is_critical() && v.reset_sync_sleep_done())
        {
            if let Some(t) = cursor.remove() {
                assert!(!t.get_mutex_wait());
                crate::processor::sched::schedule_thread(t);
            }
        } else {
            cursor.move_next();
        }
    }
}

fn do_add_to_requeue(list: &mut RBTree<RequeueLinkAdapter>, thread: ThreadRef) {
    // If already on the list, skip. This can happen with spurious wakeups.
    // The find() + insert() is protected by the caller's lock, so no TOCTOU race.
    if !list.find(&thread.objid()).is_null() {
        return;
    }
    list.insert(thread);
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
        let mut list = requeue.list.lock();
        let _ = list.find_mut(&id).remove();
        return;
    }
    log::trace!(
        "adding {} ({}) to requeue, from {}",
        thread.id(),
        thread.objid(),
        core::panic::Location::caller()
    );
    let requeue = get_requeue_list();
    let mut list = requeue.list.lock();
    do_add_to_requeue(&mut *list, thread);
}

pub fn add_all_to_requeue(iter: impl IntoIterator<Item = ThreadRef>) {
    let requeue = get_requeue_list();
    // We are going to try to enqueue all threads. Best case, we can just immediately
    // schedule the thread, but if not, enqueue it onto the requeue list for later.
    //
    // In the best-best case scenario, we don't even need to take the requeue lock.
    let mut list = None;
    for thread in iter.into_iter() {
        if !thread.is_critical() && thread.reset_sync_sleep_done() {
            assert!(!thread.get_mutex_wait());
            crate::processor::sched::schedule_thread(thread);
        } else {
            // Need to take the lock if we haven't yet.
            let list = list.get_or_insert_with(|| requeue.list.lock());
            do_add_to_requeue(&mut *list, thread);
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
        list.find_mut(&thread.objid()).remove()
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
    let start = Instant::now();
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
    let end = Instant::now();
    trace_resume(&thread, (end - start).into());
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
    let mapping = vmc
        .lookup_object(addr.try_into().map_err(|_| ArgumentError::InvalidAddress)?)
        .ok_or(ArgumentError::InvalidAddress)?;
    let offset = (addr.raw() as usize) % (1024 * 1024 * 1024); //TODO: arch-dep, centralize these calculations somewhere, see PageNumber
    Ok((mapping.object().clone(), offset, Some(addr.as_ptr())))
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
        ThreadSyncReference::Virtual(addr) => {
            get_obj_and_offset(VirtAddr::new(addr as u64).unwrap())?
        }
        ThreadSyncReference::Virtual32(addr) => {
            get_obj_and_offset(VirtAddr::new(addr as u64).unwrap())?
        }
    })
}

struct SleepEvent {
    obj: ObjectRef,
    offset: usize,
    did_sleep: bool,
}

fn prep_sleep(sleep: &ThreadSyncSleep, first_sleep: bool) -> Result<SleepEvent> {
    let (obj, offset, vaddr) = get_obj(sleep.reference)?;
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
    let (obj, offset, _) = get_obj(wake.reference)?;
    Ok(obj.wakeup_word(offset, wake.count))
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
    let start = Instant::now();
    let se = prep_sleep(&op, true)?;

    if !se.did_sleep {
        return Ok(false);
    }
    let thread = current_thread_ref().unwrap();
    let guard = thread.enter_critical();
    thread.set_sync_sleep_done();
    requeue_all();
    let prep_done = Instant::now();
    // See simple_timed_sleep: requeue_all() skips critical threads, so it can never rescue the
    // caller. Take a wakeup that raced in ahead of set_sync_sleep_done() ourselves.
    if claim_own_wakeup(&thread) {
        drop(guard);
    } else {
        finish_blocking(guard);
    }
    let woke_up = Instant::now();
    let _guard = thread.enter_critical();
    thread.reset_sync_sleep();
    thread.reset_sync_sleep_done();
    remove_from_requeue(&thread);
    drop(_guard);
    undo_sleep(&se);
    // If we have a timeout key, AND we don't find it during release, the timeout fired.
    let done = Instant::now();
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
    let start = Instant::now();
    let count = wakeup(&op)?;
    requeue_all();
    let done = Instant::now();
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

    let start = Instant::now();
    let first = ops[0];

    let mut ready_count = 0;
    let mut unsleeps = heapless::Vec::<_, 1024>::new();
    let mut num_sleepers = 0;

    for op in &mut *ops {
        match op {
            ThreadSync::Sleep(sleep, result) => match prep_sleep(sleep, unsleeps.is_empty()) {
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
            },
            ThreadSync::Wake(wake, result) => match wakeup(wake) {
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

    let prep_done = Instant::now();
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

    let woke_up = Instant::now();
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
    let done = Instant::now();
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

pub fn sys_thread_sync(ops: &mut [ThreadSync], timeout: Option<&mut Duration>) -> Result<usize> {
    let thread = current_thread_ref().unwrap();
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
