use std::sync::Mutex;

use self::{cleaner::MapCleaner, inner::MMInner};

mod cleaner;
mod handle;
mod info;
mod inner;

pub use handle::MapHandle;
pub use info::MapInfo;
pub use inner::MappedObjectAddrs;

use twizzler_runtime_api::MapError;

pub struct MapMan {
    cleaner: MapCleaner,
    inner: Mutex<MMInner>,
}

lazy_static::lazy_static! {
static ref MAPMAN: MapMan = MapMan::new();
}

impl MapMan {
    fn new() -> Self {
        fn clean_call(info: MapInfo) {
            let _unmap = match MAPMAN.inner.lock() {
                Ok(mut inner) => inner.handle_drop(info),
                Err(_) => None,
            };
            // _unmap will unmap on drop, without the mapman inner lock held.
        }
        Self {
            cleaner: MapCleaner::new(clean_call),
            inner: Mutex::new(MMInner::new()),
        }
    }
}

pub fn map_object(info: MapInfo) -> Result<MapHandle, MapError> {
    MAPMAN
        .inner
        .lock()
        .map_err(|_| MapError::InternalError)?
        .map(info)
}
