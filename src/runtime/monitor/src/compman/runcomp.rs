use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use dynlink::compartment::CompartmentId;
use talc::{ErrOnOom, Talc};
use twizzler_runtime_api::{MapError, ObjID};

use crate::mapman::{MapHandle, MapInfo};

use super::{object::CompObject, thread::CompThread};

pub(crate) struct RunCompInner {
    main_thread: CompThread,
    deps: Vec<ObjID>,
    comp_config_object: CompObject,
    // The allocator for the above object.
    allocator: Talc<ErrOnOom>,
    mapped_objects: HashMap<MapInfo, MapHandle>,
    pub sctx: ObjID,
    pub instance: ObjID,
    compartment_id: CompartmentId,
}

pub struct RunComp {
    pub sctx: ObjID,
    pub instance: ObjID,
    pub name: String,
    pub compartment_id: CompartmentId,
    inner: Arc<Mutex<RunCompInner>>,
}

impl RunCompInner {
    pub fn map_object(&mut self, info: MapInfo) -> Result<MapHandle, MapError> {
        if let Some(handle) = self.mapped_objects.get(&info) {
            return Ok(handle.clone());
        }
        let handle = crate::mapman::map_object(info)?;
        self.mapped_objects.insert(info, handle);
        self.mapped_objects
            .get(&info)
            .cloned()
            .ok_or(MapError::InternalError)
    }

    pub fn unmap_object(&mut self, info: MapInfo) {
        let _ = self.mapped_objects.remove(&info);
        // Unmap handled in MapHandle drop
    }
}

impl RunComp {
    pub fn cloned_inner(&self) -> Arc<Mutex<RunCompInner>> {
        self.inner.clone()
    }

    pub fn with_inner<R>(&self, f: impl FnOnce(&mut RunCompInner) -> R) -> R {
        let mut guard = self.inner.lock().unwrap();
        f(&mut *guard)
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}
