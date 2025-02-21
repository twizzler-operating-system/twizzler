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
                            tracing::debug!("unmapper command {:?}", info);
                            if catch_unwind(|| {
                                let monitor = get_monitor();
                                match info {
                                    UnmapCommand::SpaceUnmap(info) => {
                                        let mut space = monitor.space.lock().unwrap();
                                        space.handle_drop(info);
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
}
