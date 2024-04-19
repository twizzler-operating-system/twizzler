use std::sync::Arc;

use monitor_api::MappedObjectAddrs;

use super::{cleaner::background_unmap_info, info::MapInfo};

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
}

impl Drop for MapHandleInner {
    fn drop(&mut self) {
        // Toss this work onto a background thread.
        background_unmap_info(self.info);
    }
}
