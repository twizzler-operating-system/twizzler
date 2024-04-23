use std::{
    collections::HashMap,
    ptr::null_mut,
    sync::{atomic::AtomicPtr, Arc, Mutex},
};

use dynlink::{compartment::CompartmentId, library::LibraryId};
use monitor_api::SharedCompConfig;
use talc::{ErrOnOom, Talc};
use twizzler_runtime_api::{MapError, ObjID};

use crate::mapman::{MapHandle, MapInfo};

use super::{object::CompConfigObject, thread::CompThread};

pub(crate) struct RunCompInner {
    main_thread: Option<CompThread>,
    deps: Vec<ObjID>,
    comp_config_object: CompConfigObject,
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
    pub(crate) inner: Arc<Mutex<RunCompInner>>,
}

impl core::fmt::Debug for RunComp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunComp")
            .field("sctx", &self.sctx)
            .field("instance", &self.instance)
            .field("name", &self.name)
            .field("compartment_id", &self.compartment_id)
            .finish_non_exhaustive()
    }
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

    pub fn compartment_config(&self) -> &SharedCompConfig {
        todo!()
    }

    pub fn start_main_thread(
        &mut self,
        start: impl FnOnce() + Send + 'static,
    ) -> miette::Result<()> {
        if self.main_thread.is_some() {
            panic!("cannot start main thread in compartment twice");
        }

        self.main_thread = Some(CompThread::new(self.instance, start)?);
        Ok(())
    }

    fn new(
        sctx: ObjID,
        instance: ObjID,
        compartment_id: CompartmentId,
        root_library_id: LibraryId,
    ) -> miette::Result<Self> {
        let mapped_objects = HashMap::new();

        Ok(Self {
            main_thread: None,
            deps: Vec::new(),
            comp_config_object: CompConfigObject::new(
                instance,
                SharedCompConfig::new(sctx, null_mut()),
            )?,
            allocator: Talc::new(ErrOnOom),
            mapped_objects,
            sctx,
            instance,
            compartment_id,
        })
    }
}

impl RunComp {
    pub fn new(
        sctx: ObjID,
        instance: ObjID,
        name: impl ToString,
        dynlink_comp_id: CompartmentId,
        root_library_id: LibraryId,
    ) -> miette::Result<RunComp> {
        Ok(Self {
            sctx,
            instance,
            name: name.to_string(),
            compartment_id: dynlink_comp_id,
            inner: Arc::new(Mutex::new(RunCompInner::new(
                sctx,
                instance,
                dynlink_comp_id,
                root_library_id,
            )?)),
        })
    }

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
