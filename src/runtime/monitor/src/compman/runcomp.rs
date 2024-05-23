use std::{
    alloc::Layout,
    collections::HashMap,
    ptr::{null_mut, NonNull},
    rc::Rc,
    sync::{atomic::AtomicPtr, Arc, Mutex},
};

use dynlink::{
    compartment::CompartmentId,
    library::{CtorInfo, LibraryId},
};
use monitor_api::{SharedCompConfig, TlsTemplateInfo};
use talc::{ErrOnOom, Talc};
use twizzler_abi::upcall::UpcallFrame;
use twizzler_runtime_api::{AuxEntry, MapError, ObjID};
use twz_rt::CompartmentInitInfo;

use super::{object::CompConfigObject, stack_object::MainThreadReadyWaiter, thread::CompThread};
use crate::{
    compman::COMPMAN,
    mapman::{MapHandle, MapInfo},
};

pub(crate) struct RunCompInner {
    main_thread: Option<CompThread>,
    deps: Vec<ObjID>,
    comp_config_object: CompConfigObject,
    // The allocator for the above object.
    pub allocator: Talc<ErrOnOom>,
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
        unsafe { self.comp_config_object.get_comp_config().as_ref().unwrap() }
    }

    pub fn comp_config_object(&self) -> &CompConfigObject {
        &self.comp_config_object
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

    pub fn monitor_new<T: Copy + Sized>(&mut self, data: T) -> Result<*mut T, ()> {
        unsafe {
            let place: NonNull<T> = self.allocator.malloc(Layout::new::<T>())?.cast();
            place.as_ptr().write(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    pub fn monitor_new_slice<T: Copy + Sized>(&mut self, data: &[T]) -> Result<*mut T, ()> {
        unsafe {
            let place = self
                .allocator
                .malloc(Layout::array::<T>(data.len()).unwrap())?;
            let slice = core::slice::from_raw_parts_mut(place.as_ptr() as *mut T, data.len());
            slice.copy_from_slice(data);
            Ok(place.as_ptr() as *mut T)
        }
    }

    fn new(
        sctx: ObjID,
        instance: ObjID,
        compartment_id: CompartmentId,
        root_library_id: LibraryId,
    ) -> miette::Result<Self> {
        let mapped_objects = HashMap::new();
        let comp_config_object =
            CompConfigObject::new(instance, SharedCompConfig::new(sctx, null_mut()))?;

        let mut allocator = Talc::new(ErrOnOom);
        unsafe {
            allocator.claim(comp_config_object.alloc_span()).unwrap();
        }
        Ok(Self {
            main_thread: None,
            deps: Vec::new(),
            comp_config_object,
            allocator,
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

    pub fn start_main(
        &mut self,
        ctors: &[CtorInfo],
        entry: usize,
    ) -> miette::Result<MainThreadReadyWaiter> {
        self.with_inner(|inner| {
            let comp_config_addr = inner.comp_config_object.get_comp_config() as usize;

            let waiter = inner.main_thread.as_mut().unwrap().prep_stack_object()?;
            let ctx = self.instance;

            let ctors_in_comp = inner.monitor_new_slice(ctors).unwrap();
            let comp_init_info = CompartmentInitInfo {
                ctor_array_start: ctors_in_comp as usize,
                ctor_array_len: ctors.len(),
                comp_config_addr,
            };
            let comp_init_info_in_comp = inner.monitor_new(comp_init_info).unwrap();
            let aux_in_comp = inner
                .monitor_new_slice(&[
                    AuxEntry::RuntimeInfo(comp_init_info_in_comp as usize, 1),
                    AuxEntry::Null,
                ])
                .unwrap();
            let arg = aux_in_comp as usize;

            inner.start_main_thread(move || {
                let comp = COMPMAN.get_comp_inner(ctx).unwrap();
                let inner = comp.lock().unwrap();
                let frame = inner
                    .main_thread
                    .as_ref()
                    .unwrap()
                    .get_entry_frame(ctx, entry, arg);
                drop(inner);
                drop(comp);
                unsafe {
                    twizzler_abi::syscall::sys_thread_resume_from_upcall(&frame);
                }
            });
            Ok(waiter)
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
