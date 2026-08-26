use std::{
    panic::catch_unwind,
    time::{Duration, Instant},
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc::Sender,
        Arc,
    },
    thread::JoinHandle,
};

use twizzler_abi::syscall::{
    sys_thread_self_id, sys_thread_set_priority, PriorityClass, ThreadPriority,
};
use twizzler_rt_abi::object::ObjID;

use super::MapInfo;
use crate::mon::get_monitor;

/// Backlog depth at which the unmapper boosts itself to Realtime, and it de-boosts at zero.
///
/// Same pattern and rationale as the kernel thread reaper's self-boost: this thread runs at
/// default User priority, and a saturated machine of same-class threads starves it while every
/// dead compartment's mappings queue up behind it. The reclaim3 census measured the end state —
/// ~14k deleted objects each pinned by one never-unmapped mapping (92% of RAM) — with the
/// unmapper runnable the whole time (`sched true`, no mutex waits): pure cpu starvation, so
/// priority is the correct lever. Boost is class-level (Realtime beats any User value); the
/// work self-drains and the de-boost at empty bounds the intrusion.
const BOOST_AT: usize = 64;

/// Kill switch for the self-boost. Shipped `true`; `false` reproduces the pre-boost behaviour.
const UNMAPPER_BOOST: bool = true;

/// Backlog at which the boost is released again. Deliberately **not** zero.
///
/// `Realtime` is a **strict band**: `RunQueue::take` consults `take_realtime()` before
/// `take_timeshare()` with no aging between classes, so a runnable Realtime thread starves every
/// User thread on that cpu for as long as it stays runnable. On one cpu that is unbounded, and
/// the exit condition therefore has to be generous rather than exact.
///
/// De-boosting only at `remaining == 0` required the queue to be *empty at the instant an item
/// completes*. `CompUnmap` re-enqueues onto this same channel, so a self-feeding chain can hold
/// the backlog above zero indefinitely and the exit never fires. Measured: the smp1
/// `object_create_delete` wedge is exactly that -- 4/4 rounds, `boosts = drains + 1` every time
/// (counts 98..1940, so it is one missed exit, not a threshold), and **no monitor output at all
/// after the final boost** because the cleaner thread never ran again. Releasing once the backlog
/// is merely small leaves the tail to run at User priority, where it cannot starve anyone.
const DEBOOST_AT: usize = 8;

/// Hard ceiling on one boosted window, independent of the backlog.
///
/// Chosen, not measured. It bounds *work*, not item count: after `DeleteInstance` lands, one
/// command can be a whole `SecurityContext` teardown (region walk + TLB invalidation), so a cap
/// expressed in items would stop bounding anything real. `Instant::now` here is an rdtsc and a
/// multiply, not a syscall (see `lockdiag.rs`), so checking it per item is free.
///
/// Note the limit: this is evaluated *between* commands, so it cannot preempt a single
/// long-running one. It bounds a self-feeding stream, which is the mechanism the wedge evidence
/// supports; a wedge inside one command would need a different fix.
const BOOST_MAX: Duration = Duration::from_millis(20);

/// Commands to run un-boosted after a window expires, so the threads that were starved actually
/// get the cpu. Without it the very next command re-boosts and nothing changes.
const BOOST_COOLDOWN_ITEMS: usize = 64;

/// Lifecycle counters for the exclusive pair-handle hunt (pagerwedge.md §3.8): creates vs
/// slot-unmap enqueues. A growing gap = exclusive `MapHandle`s retained past compartment
/// unload — the class the SPACE-CENSUS split identified (kernel-pinned dead objects absent
/// from `maps`).
pub(crate) static SLOT_UNMAPS_SENT: AtomicUsize = AtomicUsize::new(0);

impl Unmapper {
    pub(crate) fn depth(&self) -> usize {
        self.backlog.load(Ordering::Relaxed)
    }
}

/// Manages a background thread that unmaps mappings.
pub struct Unmapper {
    sender: Sender<UnmapCommand>,
    /// Commands sent and not yet fully processed. Signed by senders (inc) and the worker (dec);
    /// drives the self-boost above.
    backlog: Arc<AtomicUsize>,
    _thread: JoinHandle<()>,
}

#[derive(Copy, Clone, Debug)]
pub enum UnmapCommand {
    SpaceUnmap(MapInfo),
    /// Unmap one specific slot, owned outright by the handle that enqueued it. Not routed through
    /// the MapInfo-keyed table, which cannot represent more than one mapping per object.
    SlotUnmap(usize),
    /// Drop a compartment's record of a mapping, for an unmap that arrived on a thread already
    /// holding a monitor lock and so could not take `comp_mgr` itself. This thread holds no
    /// happylock key, so it can.
    CompUnmap {
        sctx: ObjID,
        info: MapInfo,
    },
    /// Delete a compartment's instance (security context) object.
    ///
    /// Queued rather than issued at `RunComp` teardown so it lands *behind* that compartment's
    /// unmaps on this FIFO. Deleting the instance is what drives the kernel's sctx teardown, and
    /// doing it while the compartment's mappings were still installed is what let that teardown
    /// act on live regions.
    DeleteInstance(ObjID),
}

impl Unmapper {
    /// Make a new unmapper.
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let backlog = Arc::new(AtomicUsize::new(0));
        let worker_backlog = backlog.clone();
        Self {
            _thread: std::thread::Builder::new()
                .name("unmapper".to_string())
                .spawn(move || {
                    let self_id = sys_thread_self_id();
                    let mut boosted = false;
                    let mut boosted_since: Option<Instant> = None;
                    let mut cooldown = 0usize;
                    loop {
                        match receiver.recv() {
                            Ok(info) => {
                                let depth = worker_backlog.load(Ordering::Relaxed);
                                if UNMAPPER_BOOST && !boosted && cooldown == 0 && depth >= BOOST_AT {
                                    boosted = sys_thread_set_priority(
                                        self_id,
                                        // User band, not Realtime. `RunQueue::take` drains
                                        // realtime before timeshare with no aging, so a *runnable*
                                        // Realtime thread starves every User thread on its cpu --
                                        // including whatever this thread's own work is waiting on.
                                        // On smp1 that is unrecoverable: measured 6/6 smp1 wedges
                                        // with the Realtime boost on, 4/4 clean with it off, and
                                        // clean at smp4 either way. Top of the User band keeps the
                                        // reclaim rationale -- still ahead of ordinary User threads
                                        // at 64 -- inside a band the timeshare queue ages.
                                        ThreadPriority::new(PriorityClass::User, 127),
                                    )
                                    .is_ok();
                                    boosted_since = boosted.then(Instant::now);
                                    if boosted {
                                        tracing::info!(
                                            "unmapper boosted to Realtime (backlog {})",
                                            depth
                                        );
                                    }
                                }
                                if catch_unwind(|| {
                                    let monitor = get_monitor();
                                    match info {
                                        UnmapCommand::SpaceUnmap(info) => {
                                            // `handle_drop` returns the unmap rather than doing it
                                            // so the caller can release the lock first (see
                                            // `UnmapOnDrop`); dropping it inline held `space`
                                            // across `sys_object_unmap`, blocking every monitor
                                            // thread that needs it for the length of a syscall.
                                            let unmap = {
                                                let mut space = crate::lockdiag::watched(
                                                    monitor.space.lock().unwrap(),
                                                );
                                                space.handle_drop(info)
                                            };
                                            drop(unmap);
                                        }
                                        UnmapCommand::SlotUnmap(slot) => {
                                            drop(super::UnmapOnDrop::new(slot));
                                        }
                                        UnmapCommand::DeleteInstance(id) => {
                                            let _ = twizzler_abi::syscall::sys_object_ctrl(
                                                id,
                                                twizzler_abi::syscall::ObjectControlCmd::Delete(
                                                    twizzler_abi::syscall::DeleteFlags::empty(),
                                                ),
                                                0,
                                                0,
                                            )
                                            .inspect_err(|e| {
                                                tracing::warn!(
                                                    "failed to delete instance {}: {}",
                                                    id,
                                                    e
                                                )
                                            });
                                        }
                                        UnmapCommand::CompUnmap { sctx, info } => {
                                            // Re-enters the ordinary path: the handle it drops may
                                            // enqueue a `SpaceUnmap` back onto this same channel,
                                            // which the next iteration picks up.
                                            monitor.unmap_object(sctx, info);
                                        }
                                    }
                                })
                                .is_err()
                                {
                                    tracing::error!(
                                        "clean_call panicked -- exiting map cleaner thread"
                                    );
                                    break;
                                }
                                tracing::debug!("unmapper command {:?}", info);
                                let remaining = worker_backlog
                                    .fetch_sub(1, Ordering::Relaxed)
                                    .saturating_sub(1);
                                if !boosted && cooldown > 0 {
                                    cooldown -= 1;
                                }
                                let expired =
                                    boosted_since.is_some_and(|t| t.elapsed() >= BOOST_MAX);
                                if boosted && (remaining <= DEBOOST_AT || expired) {
                                    let _ = sys_thread_set_priority(self_id, ThreadPriority::USER);
                                    boosted = false;
                                    boosted_since = None;
                                    if expired {
                                        cooldown = BOOST_COOLDOWN_ITEMS;
                                        tracing::info!(
                                            "unmapper boost window expired (backlog {}), de-boosted",
                                            remaining
                                        );
                                    } else {
                                        tracing::info!(
                                            "unmapper backlog drained to {}, de-boosted",
                                            remaining
                                        );
                                    }
                                }
                            }
                            Err(_) => {
                                // If receive fails, we can't recover, but this probably doesn't
                                // happen since the sender won't get
                                // dropped since this struct is used
                                // in the MapMan static.
                                break;
                            }
                        }
                    }
                })
                .unwrap(),
            sender,
            backlog,
        }
    }

    /// Enqueue a mapping to be unmapped.
    pub(super) fn background_unmap_info(&self, info: MapInfo) {
        // If the receiver is down, this will fail, but that also shouldn't happen, unless the
        // call to clean_call above panics. In any case, handle this gracefully.
        self.backlog.fetch_add(1, Ordering::Relaxed);
        if self.sender.send(UnmapCommand::SpaceUnmap(info)).is_err() {
            self.backlog.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!("failed to enqueue Unmap {:?} onto cleaner thread", info);
        }
    }

    /// Enqueue a compartment's unmap to be performed on this thread instead of the caller's.
    pub(crate) fn background_unmap_comp(&self, sctx: ObjID, info: MapInfo) {
        self.backlog.fetch_add(1, Ordering::Relaxed);
        if self
            .sender
            .send(UnmapCommand::CompUnmap { sctx, info })
            .is_err()
        {
            self.backlog.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!(
                "failed to enqueue compartment unmap of {:?} onto cleaner thread",
                info
            );
        }
    }

    /// Enqueue a compartment instance object to be deleted, after everything already queued.
    pub(crate) fn background_delete_instance(&self, id: ObjID) {
        self.backlog.fetch_add(1, Ordering::Relaxed);
        if self.sender.send(UnmapCommand::DeleteInstance(id)).is_err() {
            self.backlog.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!("failed to enqueue delete of instance {}", id);
        }
    }

    /// Enqueue a slot to be unmapped.
    pub(super) fn background_unmap_slot(&self, slot: usize) {
        SLOT_UNMAPS_SENT.fetch_add(1, Ordering::Relaxed);
        self.backlog.fetch_add(1, Ordering::Relaxed);
        if self.sender.send(UnmapCommand::SlotUnmap(slot)).is_err() {
            self.backlog.fetch_sub(1, Ordering::Relaxed);
            tracing::warn!(
                "failed to enqueue Unmap of slot {} onto cleaner thread",
                slot
            );
        }
    }
}
