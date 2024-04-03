//! Map Cleaner
//!
//! The map cleaner thread backgrounds the task of unmapping objects from the address space. This is to offload
//! work for performance, but also to prevent unmapping during drop of a MapHandle, which may happen even if other
//! locks are held.

use std::panic::catch_unwind;
use std::{sync::mpsc::Sender, thread::JoinHandle};

use super::info::MapInfo;

pub(super) struct MapCleaner {
    sender: Sender<MapInfo>,
    thread: JoinHandle<()>,
}

impl MapCleaner {
    pub(super) fn new(clean_call: fn(MapInfo)) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            thread: std::thread::spawn(move || loop {
                match receiver.recv() {
                    Ok(info) => {
                        if let Err(_) = catch_unwind(|| clean_call(info)) {
                            tracing::error!("clean_call panicked -- exiting map cleaner thread");
                            break;
                        }
                    }
                    Err(_) => {
                        // If receive fails, we can't recover, but this probably doesn't happen since
                        // the sender won't get dropped since this struct is used in the MapMan static.
                        break;
                    }
                }
            }),
            sender,
        }
    }
}

pub(super) fn background_unmap_info(info: MapInfo) {
    // If the receiver is down, this will fail, but that also shouldn't happen, unless the
    // call to clean_call above panics. In any case, handle this gracefully.
    if super::MAPMAN.cleaner.sender.send(info).is_err() {
        tracing::warn!("failed to enqueue {:?} onto cleaner thread", info);
    }
}
