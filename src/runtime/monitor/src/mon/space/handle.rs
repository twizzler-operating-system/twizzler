use std::sync::Arc;

use monitor_api::MappedObjectAddrs;
use twizzler_abi::object::NULLPAGE_SIZE;

use super::MapInfo;
use crate::mon::get_monitor;

pub struct MapHandleInner {
    info: MapInfo,
    map: MappedObjectAddrs,
}

pub type MapHandle = Arc<MapHandleInner>;

impl MapHandleInner {
    pub(crate) fn new(info: MapInfo, map: MappedObjectAddrs) -> Self {
        Self { info, map }
    }

    pub fn addrs(&self) -> MappedObjectAddrs {
        self.map
    }

    pub fn monitor_data_null(&self) -> *mut u8 {
        self.map.start as *mut u8
    }

    pub fn monitor_data_base(&self) -> *mut u8 {
        (self.map.start + NULLPAGE_SIZE) as *mut u8
    }
}

impl Drop for MapHandleInner {
    fn drop(&mut self) {
        // Toss this work onto a background thread.
        let monitor = get_monitor();
        monitor.unmapper.background_unmap_info(self.info);
    }
}
