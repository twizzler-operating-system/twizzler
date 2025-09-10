use std::sync::Arc;

use monitor_api::MappedObjectAddrs;
use twizzler_abi::object::{ObjID, MAX_SIZE, NULLPAGE_SIZE};
use twizzler_rt_abi::object::ObjectHandle;

use super::MapInfo;
use crate::mon::get_monitor;

/// A handle for an object mapped into the address space. This handle is owning, and when dropped,
/// the mapping is sent to the background unmapping thread.
#[derive(Debug)]
pub struct MapHandleInner {
    info: MapInfo,
    map: MappedObjectAddrs,
}

/// A shared map handle.
pub type MapHandle = Arc<MapHandleInner>;

impl MapHandleInner {
    /// Create a new map handle.
    pub(crate) fn new(info: MapInfo, map: MappedObjectAddrs) -> Self {
        Self { info, map }
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
        // Toss this work onto a background thread.
        let monitor = get_monitor();
        if let Some(unmapper) = monitor.unmapper.get() {
            unmapper.background_unmap_info(self.info);
        }
    }
}
