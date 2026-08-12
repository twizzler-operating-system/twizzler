use std::{panic::catch_unwind, sync::mpsc::Sender, thread::JoinHandle};

use super::MapInfo;
use crate::mon::get_monitor;

/// Manages a background thread that unmaps mappings.
pub struct Unmapper {
    sender: Sender<UnmapCommand>,
    _thread: JoinHandle<()>,
}

#[derive(Copy, Clone, Debug)]
pub enum UnmapCommand {
    SpaceUnmap(MapInfo),
    /// Unmap one specific slot, owned outright by the handle that enqueued it. Not routed through
    /// the MapInfo-keyed table, which cannot represent more than one mapping per object.
    SlotUnmap(usize),
}

impl Unmapper {
    /// Make a new unmapper.
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            _thread: std::thread::Builder::new()
                .name("unmapper".to_string())
                .spawn(move || loop {
                    match receiver.recv() {
                        Ok(info) => {
                            if catch_unwind(|| {
                                let monitor = get_monitor();
                                match info {
                                    UnmapCommand::SpaceUnmap(info) => {
                                        let mut space =
                                            crate::lockdiag::watched(monitor.space.lock().unwrap());
                                        space.handle_drop(info);
                                    }
                                    UnmapCommand::SlotUnmap(slot) => {
                                        drop(super::UnmapOnDrop::new(slot));
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
                        }
                        Err(_) => {
                            // If receive fails, we can't recover, but this probably doesn't happen
                            // since the sender won't get dropped since this
                            // struct is used in the MapMan static.
                            break;
                        }
                    }
                })
                .unwrap(),
            sender,
        }
    }

    /// Enqueue a mapping to be unmapped.
    pub(super) fn background_unmap_info(&self, info: MapInfo) {
        // If the receiver is down, this will fail, but that also shouldn't happen, unless the
        // call to clean_call above panics. In any case, handle this gracefully.
        if self.sender.send(UnmapCommand::SpaceUnmap(info)).is_err() {
            tracing::warn!("failed to enqueue Unmap {:?} onto cleaner thread", info);
        }
    }

    /// Enqueue a slot to be unmapped.
    pub(super) fn background_unmap_slot(&self, slot: usize) {
        if self.sender.send(UnmapCommand::SlotUnmap(slot)).is_err() {
            tracing::warn!(
                "failed to enqueue Unmap of slot {} onto cleaner thread",
                slot
            );
        }
    }
}
