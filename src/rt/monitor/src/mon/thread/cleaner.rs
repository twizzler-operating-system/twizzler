use std::{
    collections::HashMap,
    marker::PhantomPinned,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{Receiver, Sender},
        Arc,
    },
    time::Duration,
};

use secgate::TwzError;
use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::object::ObjID;

use super::ManagedThread;
use crate::mon::get_monitor;

/// Queued-but-undrained ops past which [`ThreadCleanerData::notify`] wakes the cleaner even though
/// it looks awake.
///
/// The parked check below is exact for a cleaner that is merely busy, since a busy cleaner
/// re-drains before it parks. It is not exact for one that is *stuck* -- blocked on the lock
/// collection behind a compartment load, say -- because "awake" then means "not going to look at
/// the queue for a while", and every thread queued in that window has nobody watching for its exit.
/// Waking past a depth bounds that: the syscall is wasted on a running cleaner, but a wasted wake
/// is cheaper than an unwatched thread.
const WAKE_DEPTH: usize = 8;

/// A/B switch: `false` issues the wake syscall on every queued op, the way this used to.
const COALESCE_WAKES: bool = true;

/// Tracks threads that do not exit cleanly, so their monitor-internal resources can be cleaned up.
pub(crate) struct ThreadCleaner {
    _thread: std::thread::JoinHandle<()>,
    send: Sender<WaitOp>,
    inner: Pin<Arc<ThreadCleanerData>>,
}

#[derive(Default)]
struct ThreadCleanerData {
    notify: AtomicU64,
    /// Set while the cleaner is (about to be) blocked in `sys_thread_sync`.
    parked: AtomicBool,
    /// [`WaitOp`]s sent but not yet drained.
    pending: AtomicUsize,
    _unpin: PhantomPinned,
}

// All the threads we are tracking.
struct Waits {
    /// Tracked threads. Parallel to `ops[1..]`.
    entries: Vec<ManagedThread>,
    /// Position of each thread in `entries`.
    idx: HashMap<ObjID, usize>,
    /// The sleep ops handed to `sys_thread_sync`, maintained in step with `entries` rather than
    /// rebuilt: `ops[0]` waits on the notify word, and `ops[i + 1]` on `entries[i]`'s exit.
    ///
    /// Rebuilding this from scratch made every wakeup O(tracked threads) in userspace on top of
    /// the O(tracked threads) the kernel already pays inserting the sleep entries -- and the
    /// cleaner wakes once per spawn. The kernel half is inherent to waiting on N words with
    /// one syscall (`sysperf.md` lead 3); this half was not.
    ops: Vec<ThreadSync>,
}

// Changes to the collection of threads we are tracking
enum WaitOp {
    Add(ManagedThread),
    Remove(ObjID),
}

impl ThreadCleaner {
    /// Makes a new ThreadCleaner.
    pub(crate) fn new() -> Self {
        let (send, recv) = std::sync::mpsc::channel();
        let data = Arc::pin(ThreadCleanerData::default());
        let inner = data.clone();
        let thread = std::thread::Builder::new()
            .name("thread-exit cleanup tracker".into())
            .spawn(move || cleaner_thread_main(data, recv))
            .unwrap();
        Self {
            send,
            inner,
            _thread: thread,
        }
    }

    /// Track a thread. If that thread exits, the cleanup thread will remove the exited thread from
    /// tracking and from the global thread manager.
    pub fn track(&self, th: ManagedThread) {
        tracing::debug!("tracking thread {}", th.id);
        let depth = self.queued();
        let _ = self.send.send(WaitOp::Add(th));
        self.inner.notify(depth >= WAKE_DEPTH);
    }

    /// Untrack a thread. Threads removed this way do not trigger a removal from the global thread
    /// manager.
    pub fn untrack(&self, id: ObjID) {
        let depth = self.queued();
        let _ = self.send.send(WaitOp::Remove(id));
        self.inner.notify(depth >= WAKE_DEPTH);
    }

    /// Count an op *before* it is sent, and report the resulting depth.
    ///
    /// Before the send, not after: the cleaner decrements this as it drains, and if it drained an
    /// op that had not been counted yet the counter would underflow -- leaving `depth` permanently
    /// past [`WAKE_DEPTH`] and every spawn back to issuing a wake syscall, silently.
    fn queued(&self) -> usize {
        self.inner.pending.fetch_add(1, Ordering::SeqCst) + 1
    }
}

impl ThreadCleanerData {
    /// Notify the cleanup thread that new items are on the queue.
    ///
    /// The wake syscall is only needed when the cleaner is parked, and this was issuing one per
    /// spawn -- which is what made `register` cost 45-78 us on smp4 against 3-13 on smp1: the wake
    /// lands the cleaner on another cpu, rebuilding its wait set and issuing a `sys_thread_sync`
    /// concurrently with the spawn that woke it.
    ///
    /// Skipping it when the cleaner is awake cannot lose the notification. The two stores are
    /// ordered against each other: the cleaner stores `parked` before reading `notify`, and this
    /// stores `notify` before reading `parked`, so at least one of them sees the other's store. If
    /// the cleaner reads `notify` first it finds 1 and does not park; if this reads `parked` first
    /// it finds `true` and wakes it. The sleep op is itself "sleep while notify == 0", evaluated by
    /// the kernel at sleep time, which closes the remaining window between the read and the
    /// syscall.
    fn notify(&self, force: bool) {
        self.notify.store(1, Ordering::SeqCst);
        if COALESCE_WAKES && !force && !self.parked.load(Ordering::SeqCst) {
            return;
        }
        let mut ops = [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&self.notify),
            1,
        ))];
        if let Err(e) = sys_thread_sync(&mut ops, None) {
            tracing::warn!("thread sync error when trying to notify: {}", e);
        }
    }
}

impl Waits {
    /// A wait set holding only the notify op.
    fn new(notify: &AtomicU64) -> Self {
        Self {
            entries: Vec::new(),
            idx: HashMap::new(),
            ops: vec![ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(notify),
                0,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
        }
    }

    fn add(&mut self, th: ManagedThread) {
        let op = ThreadSync::new_sleep(th.waitable_until_exit());
        match self.idx.get(&th.id).copied() {
            Some(i) => {
                self.entries[i] = th;
                self.ops[i + 1] = op;
            }
            None => {
                self.idx.insert(th.id, self.entries.len());
                self.entries.push(th);
                self.ops.push(op);
            }
        }
    }

    fn remove(&mut self, id: ObjID) -> Option<ManagedThread> {
        let i = self.idx.remove(&id)?;
        let th = self.entries.swap_remove(i);
        self.ops.swap_remove(i + 1);
        // `swap_remove` moved the last entry into this slot, in both vectors alike.
        if let Some(moved) = self.entries.get(i) {
            self.idx.insert(moved.id, i);
        }
        Some(th)
    }

    /// Move every exited thread out of the wait set.
    fn take_exited(&mut self, out: &mut Vec<ManagedThread>) {
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].has_exited() {
                let id = self.entries[i].id;
                if let Some(th) = self.remove(id) {
                    out.push(th);
                }
                // `remove` swapped a different thread into `i`, so do not advance.
            } else {
                i += 1;
            }
        }
    }

    fn process_queue(&mut self, recv: &mut Receiver<WaitOp>, data: &ThreadCleanerData) -> bool {
        let mut did_work = false;
        while let Ok(wo) = recv.try_recv() {
            did_work = true;
            data.pending.fetch_sub(1, Ordering::SeqCst);
            match wo {
                WaitOp::Add(th) => self.add(th),
                WaitOp::Remove(id) => {
                    self.remove(id);
                }
            }
        }
        did_work
    }
}

fn cleaner_thread_main(data: Pin<Arc<ThreadCleanerData>>, mut recv: Receiver<WaitOp>) {
    // TODO (dbittman): when we have support for async thread events, we can use that API.
    let mut cleanups = Vec::new();
    let mut waits = Waits::new(&data.notify);
    // Self-boost while compartment teardown is backlogged, same pattern as the kernel thread
    // reaper (and the unmapper). This thread runs teardown at User priority against whatever
    // storm produced the dead compartments, and it loses: reclaim3 measured ~15k queued dead
    // compartments' contexts pinning 92% of RAM, with the cleaner runnable the whole time and
    // no stuck-thread warnings — production simply outran consumption. Realtime while the
    // backlog is deep, back to normal when it clears.
    const BOOST_AT: usize = 32;
    let self_id = twizzler_abi::syscall::sys_thread_self_id();
    let mut boosted = false;
    let mut diag_tick = 0u32;
    loop {
        // COMP-CENSUS: name the population stuck upstream of the cleanup queue. reclaim5 showed
        // teardown firing for only ~112 of ~16k dead compartments — the rest sit at
        // use_count > 0, i.e. someone never dropped a handle. This says who and at what count.
        diag_tick += 1;
        // Gated whole: skipping the census also skips its lock traffic, not just the lines.
        if diag_tick % 64 == 0 && crate::diag_enabled() {
            let monitor = get_monitor();
            let key = happylock::ThreadKey::get().unwrap();
            let cmgr = crate::lockdiag::watched(monitor.comp_mgr.read(key));
            let mut total = 0usize;
            let mut exited = 0usize;
            let mut held = 0usize;
            let mut samples: Vec<(twizzler_rt_abi::object::ObjID, String, u64)> = Vec::new();
            for rc in cmgr.compartments() {
                total += 1;
                if rc.has_flag(crate::mon::compartment::COMP_EXITED) {
                    exited += 1;
                    if rc.use_count > 0 {
                        held += 1;
                        if samples.len() < 4 {
                            samples.push((rc.instance, rc.name.clone(), rc.use_count));
                        }
                    }
                }
            }
            drop(cmgr);
            // SPACE-CENSUS: the maps table split. The kernel census shows dead objects pinned
            // by never-issued unmaps; whether those objects are still *in* `maps` with a
            // nonzero count (a counted-handle leak) or absent (loader-side exclusive/pair
            // handles never dropped) picks the final suspect.
            {
                let monitor = get_monitor();
                if let Ok(space) = monitor.space.try_lock() {
                    let (total, active, top) = space.census();
                    tracing::info!(
                        "SPACE-CENSUS: {} maps, {} active; top counts: {:?}",
                        total,
                        active,
                        top
                    );
                }
                // The pair-handle pipeline: creates vs releases, plus where teardown stands.
                // made-sent = exclusive handles still alive; cleanups vs drops localizes a
                // stall between unload and RunComp destruction.
                tracing::info!(
                    "PAIR-CENSUS: pair-handles made {} slot-unmaps sent {} unmapper-depth {} cleanups {} runcomp-drops {}",
                    crate::mon::space::PAIR_HANDLES_MADE.load(Ordering::Relaxed),
                    crate::mon::space::SLOT_UNMAPS_SENT.load(Ordering::Relaxed),
                    monitor.unmapper.get().map(|u| u.depth()).unwrap_or(0),
                    crate::mon::compartment::CLEANUPS_DONE.load(Ordering::Relaxed),
                    crate::mon::compartment::RUNCOMP_DROPS.load(Ordering::Relaxed)
                );
            }
            if held > 0 {
                tracing::info!(
                    "COMP-CENSUS: {} compartments, {} exited, {} exited-but-held",
                    total,
                    exited,
                    held
                );
                for (id, name, uc) in samples {
                    tracing::info!("  held: {} '{}' use_count {}", id, name, uc);
                }
            }
        }
        let backlog = super::super::compartment::CLEANUP_BACKLOG.load(Ordering::Relaxed);
        if !boosted && backlog >= BOOST_AT {
            boosted = twizzler_abi::syscall::sys_thread_set_priority(
                self_id,
                twizzler_abi::syscall::ThreadPriority::new(
                    twizzler_abi::syscall::PriorityClass::Realtime,
                    64,
                ),
            )
            .is_ok();
            if boosted {
                tracing::info!("cleaner boosted to Realtime (cleanup backlog {})", backlog);
            }
        } else if boosted && backlog == 0 {
            let _ = twizzler_abi::syscall::sys_thread_set_priority(
                self_id,
                twizzler_abi::syscall::ThreadPriority::USER,
            );
            boosted = false;
            tracing::info!("cleanup backlog drained, cleaner de-boosted");
        }
        // Apply any waiting operations.
        let mut did_work = waits.process_queue(&mut recv, &data);

        waits.take_exited(&mut cleanups);
        if !cleanups.is_empty() {
            did_work = true;
        }
        // Remove any exited threads from the thread manager.
        for th in cleanups.drain(..) {
            tracing::debug!("cleaning thread: {}", th.id);
            let monitor = get_monitor();
            {
                let key = happylock::ThreadKey::get().unwrap();
                let mut tmgr = crate::lockdiag::watched(monitor.thread_mgr.write(key));
                tmgr.do_remove(&th);
            }
            let comps = {
                let key = happylock::ThreadKey::get().unwrap();
                let (ref tmgr, ref mut cmgr, ref mut dynlink, _, _) =
                    *crate::lockdiag::watched(monitor.locks.lock(key));
                for comp in cmgr.compartments_mut() {
                    comp.clean_per_thread_data(th.id);
                }
                if let Some(comp_id) = th.main_thread_comp {
                    let others = tmgr.threads_of(comp_id);
                    cmgr.main_thread_exited(comp_id, &others);
                }
                cmgr.process_cleanup_queue(tmgr, &mut *dynlink)
            };
            drop(comps);
        }

        // Build the next spawn's supervisor TLS region here rather than on that spawn's own
        // critical path. This thread is the one place in the monitor that can take the lock
        // collection with nobody waiting on the result.
        super::refill_ready_tls();

        // Check for notifications, and sleep. `parked` is stored before `notify` is read, which is
        // what lets `ThreadCleanerData::notify` skip its syscall while this thread is awake.
        data.parked.store(true, Ordering::SeqCst);
        if !did_work && data.notify.swap(0, Ordering::SeqCst) == 0 {
            // no notification, go to sleep. hold the lock over the sleep so that someone cannot
            // modify waits.threads on us while we're asleep.
            if let Err(e) = sys_thread_sync(&mut waits.ops, Some(Duration::from_secs(8))) {
                if e != TwzError::TIMED_OUT {
                    tracing::warn!("thread sync error: {}", e);
                }
            }
        }
        data.parked.store(false, Ordering::SeqCst);
    }
}
