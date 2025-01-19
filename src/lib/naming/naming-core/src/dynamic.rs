use secgate::{DynamicSecGate, util::{Descriptor, Handle, SimpleBuffer}, SecGateReturn};
use twizzler_rt_abi::object::ObjID;
use monitor_api::CompartmentHandle;
use std::sync::OnceLock;
use crate::NamingHandle;
use crate::api::NamerAPI;

pub struct DynamicNamerAPI {
    _handle: &'static CompartmentHandle,
    put: DynamicSecGate<'static, (Descriptor, ), ()>,
    get: DynamicSecGate<'static, (Descriptor, ), Option<u128>>,
    open_handle: DynamicSecGate<'static, (), Option<(Descriptor, ObjID)>>,
    close_handle: DynamicSecGate<'static, (Descriptor, ), ()>,
    enumerate_names: DynamicSecGate<'static, (Descriptor, ), Option<usize>>,
    remove: DynamicSecGate<'static, (Descriptor, ), ()>,
}

impl NamerAPI for DynamicNamerAPI {
    fn put(&self, desc: Descriptor) -> SecGateReturn<()> {
        (self.put)(desc)
    }

    fn get(&self, desc: Descriptor) -> SecGateReturn<Option<u128>> {
        (self.get)(desc)
    }

    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>> {
        (self.open_handle)()
    }

    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()> {
        (self.close_handle)(desc)
    }

    fn enumerate_names(&self, desc: Descriptor) -> SecGateReturn<Option<usize>> {
        (self.enumerate_names)(desc)
    }

    fn remove(&self, desc: Descriptor) -> SecGateReturn<()> {
        (self.remove)(desc)
    }
}

static DYNAMIC_NAMER_API: OnceLock<DynamicNamerAPI> = OnceLock::new();
 
pub fn dynamic_namer_api() -> &'static DynamicNamerAPI {
    DYNAMIC_NAMER_API.get_or_init(|| {
        let handle = Box::leak(Box::new(
            CompartmentHandle::lookup("naming").expect("failed to open namer compartment"),
        ));
        DynamicNamerAPI {
            _handle: handle,
            put: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, ), ()>("put")
                    .expect("failed to find put gate call")
            },
            get: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, ), Option<u128>>("get")
                    .expect("failed to find get gate call")
            },
            open_handle: unsafe {
                handle
                    .dynamic_gate::<(), Option<(Descriptor, ObjID)>>("open_handle")
                    .expect("failed to find open_handle gate call")
            },
            close_handle: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, ), ()>("close_handle")
                    .expect("failed to find close_handle gate call")
            },
            enumerate_names: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, ), Option<usize>>("enumerate_names")
                    .expect("failed to find enumerate_names gate call")
            },
            remove: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, ), ()>("remove")
                    .expect("failed to find remove gate call")
            },
        }
    })
}

pub type DynamicNamingHandle = NamingHandle<'static, DynamicNamerAPI>;

pub fn dynamic_naming_factory() -> Option<DynamicNamingHandle> {
    NamingHandle::new(dynamic_namer_api())
}