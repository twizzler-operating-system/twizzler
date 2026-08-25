use std::{
    panic::catch_unwind,
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
                    loop {
                        match receiver.recv() {
                            Ok(info) => {
                                let depth = worker_backlog.load(Ordering::Relaxed);
                                if !boosted && depth >= BOOST_AT {
                                    boosted = sys_thread_set_priority(
                                        self_id,
                                        ThreadPriority::new(PriorityClass::Realtime, 64),
                                    )
                                    .is_ok();
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
                                            let mut space = crate::lockdiag::watched(
                                                monitor.space.lock().unwrap(),
                                            );
                                            space.handle_drop(info);
                                        }
                                        UnmapCommand::SlotUnmap(slot) => {
                                            drop(super::UnmapOnDrop::new(slot));
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
                                if boosted && remaining == 0 {
                                    let _ = sys_thread_set_priority(self_id, ThreadPriority::USER);
                                    boosted = false;
                                    tracing::info!("unmapper backlog drained, de-boosted");
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
