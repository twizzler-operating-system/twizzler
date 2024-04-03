use std::{
    cell::OnceCell,
    collections::HashMap,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

use dynlink::compartment::CompartmentId;
use monitor_api::SharedCompConfig;
use talc::{ErrOnOom, Talc};
use twizzler_runtime_api::{MapError, ObjID};

use crate::mapman::{MapHandle, MapInfo, MappedObjectAddrs};

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
    name: String,
    compartment_id: CompartmentId,
    inner: Arc<Mutex<RunCompInner>>,
}

impl RunCompInner {
    pub fn map_object(&mut self, info: MapInfo) -> Result<MappedObjectAddrs, MapError> {
        if let Some(handle) = self.mapped_objects.get(&info) {
            Ok(handle.addrs())
        } else {
            let handle = crate::mapman::map_object(info)?;
            self.mapped_objects.insert(info, handle);
            self.mapped_objects
                .get(&info)
                .ok_or(MapError::InternalError)
                .map(|h| h.addrs())
        }
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

    pub fn name(&self) -> &str {
        &self.name
    }
}
