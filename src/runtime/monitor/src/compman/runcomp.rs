use std::{
    alloc::Layout,
    collections::HashMap,
    ptr::{null_mut, NonNull},
    rc::Rc,
    sync::{
        atomic::{AtomicPtr, AtomicU64, Ordering},
        Arc, Mutex, Once,
    },
};

use dynlink::{
    compartment::CompartmentId,
    library::{CtorInfo, LibraryId},
};
use monitor_api::{SharedCompConfig, TlsTemplateInfo};
use talc::{ErrOnOom, Talc};
use twizzler_abi::syscall::{
    ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference, ThreadSyncSleep,
};
use twizzler_runtime_api::{AuxEntry, MapError, ObjID};
use twz_rt::CompartmentInitInfo;

use super::{object::CompConfigObject, thread::CompThread};
use crate::{
    compman::COMPMAN,
    mapman::{MapHandle, MapInfo},
};

const COMP_READY: u64 = 0x1;

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
    pub flags: AtomicU64,
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
    pub fn start_main(&mut self, ctors: &[CtorInfo], entry: usize) -> miette::Result<()> {
        let comp_config_addr = self.comp_config_object.get_comp_config() as usize;

        let ctx = self.instance;

        let ctors_in_comp = self.monitor_new_slice(ctors).unwrap();
        let comp_init_info = CompartmentInitInfo {
            ctor_array_start: ctors_in_comp as usize,
            ctor_array_len: ctors.len(),
            comp_config_addr,
        };
        let comp_init_info_in_comp = self.monitor_new(comp_init_info).unwrap();
        let aux_in_comp = self
            .monitor_new_slice(&[
                AuxEntry::RuntimeInfo(comp_init_info_in_comp as usize, 1),
                AuxEntry::Null,
            ])
            .unwrap();
        let arg = aux_in_comp as usize;

        self.start_main_thread(move || {
            tracing::info!("==> {}", ctx);
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
        })?;
        Ok(())
    }

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
            flags: AtomicU64::new(0),
        })
    }

    pub fn set_ready(&self) {
        self.flags.fetch_or(COMP_READY, Ordering::SeqCst);
    }

    pub fn is_ready(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & COMP_READY != 0
    }

    pub fn ready_waitable(&self) -> Option<ThreadSyncSleep> {
        let flags = self.flags.load(Ordering::SeqCst);
        if flags & COMP_READY == 0 {
            Some(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual(&self.flags),
                flags,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))
        } else {
            None
        }
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

    pub fn ready_waiter(&self) -> RunCompReadyWaiter {
        RunCompReadyWaiter {
            rc: self.inner.clone(),
        }
    }
}

pub struct RunCompReadyWaiter {
    rc: Arc<Mutex<RunCompInner>>,
}

impl RunCompReadyWaiter {
    pub fn wait(&self) {
        loop {
            let wait = { self.rc.lock().unwrap().ready_waitable() };
            let Some(wait) = wait else {
                break;
            };

            if let Err(e) =
                twizzler_abi::syscall::sys_thread_sync(&mut [ThreadSync::new_sleep(wait)], None)
            {
                tracing::warn!("thread sync error: {:?}", e);
            }
        }
    }
}
