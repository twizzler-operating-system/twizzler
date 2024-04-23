use std::sync::{Mutex, Once, OnceLock};

use self::{cleaner::MapCleaner, inner::MMInner};

mod cleaner;
mod handle;
mod info;
mod inner;

pub use handle::MapHandle;
pub use info::MapInfo;

use miette::IntoDiagnostic;
use twizzler_abi::syscall::{
    sys_object_create, sys_object_ctrl, CreateTieSpec, DeleteFlags, ObjectControlCmd, ObjectCreate,
    ObjectSource,
};
use twizzler_runtime_api::{MapError, MapFlags};

pub struct MapMan {
    cleaner: OnceLock<MapCleaner>,
    inner: Mutex<MMInner>,
}

lazy_static::lazy_static! {
static ref MAPMAN: MapMan = MapMan::new();
}

impl MapMan {
    fn new() -> Self {
        Self {
            cleaner: OnceLock::new(),
            inner: Mutex::new(MMInner::new()),
        }
    }

    fn start_cleaner(&self) {
        fn clean_call(info: MapInfo) {
            let _unmap = match MAPMAN.inner.lock() {
                Ok(mut inner) => inner.handle_drop(info),
                Err(_) => None,
            };
            // _unmap will unmap on drop, without the mapman inner lock held.
        }
        if self.cleaner.set(MapCleaner::new(clean_call)).is_err() {
            panic!("cannot start map cleaner thread multiple times");
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

pub(crate) fn safe_create_and_map_object(
    spec: ObjectCreate,
    sources: &[ObjectSource],
    ties: &[CreateTieSpec],
    map_flags: MapFlags,
) -> miette::Result<MapHandle> {
    let id = sys_object_create(spec, sources, ties).into_diagnostic()?;

    match map_object(MapInfo {
        id,
        flags: map_flags,
    }) {
        Ok(mh) => Ok(mh),
        Err(me) => {
            if let Err(e) = sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty())) {
                tracing::warn!("failed to delete object {} after map failure {}", e, me);
            }
            Err(me)
        }
    }
    .into_diagnostic()
}
