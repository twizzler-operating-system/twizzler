use std::collections::HashMap;

use dynlink::compartment::CompartmentId;
use monitor_api::SharedCompConfig;
use twizzler_abi::{
    object::NULLPAGE_SIZE,
    syscall::{sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags},
};
use twizzler_object::ObjID;
use twizzler_runtime_api::{MapFlags, ObjectHandle};

pub struct Comp {
    pub sctx_id: ObjID,
    pub compartment_id: CompartmentId,
    pub comp_config_obj: ObjectHandle,

    thread_map: HashMap<ObjID, CompThreadInfo>,
}

impl core::fmt::Debug for Comp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Comp({:x}, {})", &self.sctx_id, &self.compartment_id)
    }
}

pub(crate) fn make_new_comp_config_object() -> ObjectHandle {
    let id = sys_object_create(
        ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        ),
        &[],
        &[],
    )
    .unwrap();

    twizzler_runtime_api::get_runtime()
        .map_object(id.as_u128(), MapFlags::READ | MapFlags::WRITE)
        .unwrap()
}

impl Comp {
    pub fn new(sctx_id: ObjID, compartment_id: CompartmentId) -> Self {
        Self {
            sctx_id,
            compartment_id,
            comp_config_obj: make_new_comp_config_object(),
            thread_map: Default::default(),
        }
    }

    pub fn get_thread_info(&mut self, thid: ObjID) -> &mut CompThreadInfo {
        self.thread_map
            .entry(thid)
            .or_insert_with(|| CompThreadInfo::new(thid))
    }

    pub fn get_comp_config(&self) -> *const SharedCompConfig {
        unsafe { self.comp_config_obj.start.add(NULLPAGE_SIZE) as *const _ }
    }
}

pub struct CompThreadInfo {
    pub thread_id: ObjID,
    pub stack_obj: Option<ObjectHandle>,
    pub thread_ptr: usize,
}

impl CompThreadInfo {
    pub fn new(thread_id: ObjID) -> Self {
        Self {
            thread_id,
            stack_obj: None,
            thread_ptr: 0,
        }
    }
}
