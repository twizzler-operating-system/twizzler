use std::sync::{atomic::AtomicU64, Arc};

use monitor_api::MappedObjectAddrs;
use twizzler_abi::{
    object::{ObjID, MAX_SIZE, NULLPAGE_SIZE},
    syscall::sys_object_remove_note,
};
use twizzler_rt_abi::object::ObjectHandle;

use super::MapInfo;
use crate::mon::get_monitor;

/// A handle for an object mapped into the address space. This handle is owning, and when dropped,
/// the mapping is sent to the background unmapping thread.
#[derive(Debug)]
pub struct MapHandleInner {
    info: MapInfo,
    map: MappedObjectAddrs,
    map_note_key: AtomicU64,
    /// True if this handle is the sole owner of its slot (see `Space::map_pair`), in which case
    /// dropping it unmaps that slot directly instead of going through the shared, MapInfo-keyed
    /// reference count in `Space::maps`.
    exclusive: bool,
}

/// A shared map handle.
pub type MapHandle = Arc<MapHandleInner>;

impl MapHandleInner {
    /// Create a new map handle.
    pub(crate) fn new(info: MapInfo, map: MappedObjectAddrs) -> Self {
        Self {
            info,
            map,
            map_note_key: AtomicU64::new(0),
            exclusive: false,
        }
    }

    /// Create a map handle that solely owns its slot.
    pub(crate) fn new_exclusive(info: MapInfo, map: MappedObjectAddrs) -> Self {
        Self {
            info,
            map,
            map_note_key: AtomicU64::new(0),
            exclusive: true,
        }
    }

    pub(crate) fn set_map_note_key(&self, key: u64) {
        self.map_note_key
            .store(key, std::sync::atomic::Ordering::SeqCst);
    }

    pub(crate) fn get_map_note_key(&self) -> u64 {
        self.map_note_key.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the mapped addresses of this handle.
    pub fn addrs(&self) -> MappedObjectAddrs {
        self.map
    }

    /// Get a pointer to the start address of the object.
    pub fn monitor_data_start(&self) -> *mut u8 {
        self.map.start as *mut u8
    }

    /// Get a pointer to the base address of the object.
    pub fn monitor_data_base(&self) -> *mut u8 {
        (self.map.start + NULLPAGE_SIZE) as *mut u8
    }

    pub fn id(&self) -> ObjID {
        self.info.id
    }

    pub unsafe fn object_handle(&self) -> ObjectHandle {
        ObjectHandle::new(
            self.info.id,
            core::ptr::null_mut(),
            self.map.start as *mut _,
            self.map.meta as *mut _,
            self.info.flags,
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }
}

impl Drop for MapHandleInner {
    fn drop(&mut self) {
        let nk = self.get_map_note_key();
        if nk != 0 {
            let _ = sys_object_remove_note(self.info.id, nk);
        }
        // Toss this work onto a background thread.
        let monitor = get_monitor();
        if let Some(unmapper) = monitor.unmapper.get() {
            if self.exclusive {
                unmapper.background_unmap_slot(self.map.slot);
            } else {
                unmapper.background_unmap_info(self.info);
            }
        }
    }
}
