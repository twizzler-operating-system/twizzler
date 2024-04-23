use monitor_api::SharedCompConfig;
use twizzler_abi::syscall::{
    BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate, ObjectCreateFlags,
};
use twizzler_runtime_api::{MapFlags, ObjID};

use crate::mapman::{safe_create_and_map_object, MapHandle};

pub struct CompConfigObject {
    handle: MapHandle,
}

impl CompConfigObject {
    pub fn new(instance: ObjID, init_val: SharedCompConfig) -> miette::Result<Self> {
        let cs = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            Some(instance),
            ObjectCreateFlags::empty(),
        );
        let handle = safe_create_and_map_object(
            cs,
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty())],
            MapFlags::READ | MapFlags::WRITE,
        )?;

        let this = Self { handle };
        this.write_config(init_val);

        Ok(this)
    }

    pub fn write_config(&self, val: SharedCompConfig) {
        unsafe {
            let base = self.handle.monitor_data_base();
            (base as *mut SharedCompConfig).write(val);
        }
    }

    pub fn get_comp_config(&self) -> *const SharedCompConfig {
        self.handle.monitor_data_base() as *const SharedCompConfig
    }
}
