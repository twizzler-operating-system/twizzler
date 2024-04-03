use std::{cell::OnceCell, collections::HashMap, ptr::NonNull};

use dynlink::compartment::CompartmentId;
use monitor_api::SharedCompConfig;
use talc::{ErrOnOom, Talc};
use twizzler_runtime_api::{MapError, ObjID};

use crate::mapman::{MapHandle, MapInfo, MappedObjectAddrs};

use super::{object::CompObject, thread::CompThread};

pub struct RunComp {
    main_thread: CompThread,
    pub sctx: ObjID,
    pub instance: ObjID,
    compartment_id: CompartmentId,
    deps: Vec<ObjID>,
    comp_config_object: CompObject,
    // The allocator for the above object.
    allocator: Talc<ErrOnOom>,
    // The base config data for the compartment, located within the alloc object.
    //comp_config: OnceCell<NonNull<SharedCompConfig>>,
    mapped_objects: HashMap<MapInfo, MapHandle>,
}

impl RunComp {
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

    pub fn name(&self) -> &str {
        todo!()
    }
}
