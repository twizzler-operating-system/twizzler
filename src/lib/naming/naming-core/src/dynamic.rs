use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::{util::Descriptor, DynamicSecGate, SecGateReturn};
use twizzler_rt_abi::object::ObjID;

use crate::{api::NamerAPI, handle::NamingHandle, Entry, Result};

pub struct DynamicNamerAPI {
    _handle: &'static CompartmentHandle,
    put: DynamicSecGate<'static, (Descriptor,), Result<()>>,
    get: DynamicSecGate<'static, (Descriptor,), Result<Entry>>,
    open_handle: DynamicSecGate<'static, (), Option<(Descriptor, ObjID)>>,
    close_handle: DynamicSecGate<'static, (Descriptor,), ()>,
    enumerate_names: DynamicSecGate<'static, (Descriptor,), Result<usize>>,
    remove: DynamicSecGate<'static, (Descriptor, bool), Result<()>>,
    change_namespace: DynamicSecGate<'static, (Descriptor,), Result<()>>,
}

impl NamerAPI for DynamicNamerAPI {
    fn put(&self, desc: Descriptor) -> SecGateReturn<Result<()>> {
        (self.put)(desc)
    }

    fn get(&self, desc: Descriptor) -> SecGateReturn<Result<Entry>> {
        (self.get)(desc)
    }

    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>> {
        (self.open_handle)()
    }

    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()> {
        (self.close_handle)(desc)
    }

    fn enumerate_names(&self, desc: Descriptor) -> SecGateReturn<Result<usize>> {
        (self.enumerate_names)(desc)
    }

    fn remove(&self, desc: Descriptor, recursive: bool) -> SecGateReturn<Result<()>> {
        (self.remove)(desc, recursive)
    }

    fn change_namespace(&self, desc: Descriptor) -> SecGateReturn<Result<()>> {
        (self.change_namespace)(desc)
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
                    .dynamic_gate::<(Descriptor,), Result<()>>("put")
                    .expect("failed to find put gate call")
            },
            get: unsafe {
                handle
                    .dynamic_gate::<(Descriptor,), Result<Entry>>("get")
                    .expect("failed to find get gate call")
            },
            open_handle: unsafe {
                handle
                    .dynamic_gate::<(), Option<(Descriptor, ObjID)>>("open_handle")
                    .expect("failed to find open_handle gate call")
            },
            close_handle: unsafe {
                handle
                    .dynamic_gate::<(Descriptor,), ()>("close_handle")
                    .expect("failed to find close_handle gate call")
            },
            enumerate_names: unsafe {
                handle
                    .dynamic_gate::<(Descriptor,), Result<usize>>("enumerate_names")
                    .expect("failed to find enumerate_names gate call")
            },
            remove: unsafe {
                handle
                    .dynamic_gate::<(Descriptor, bool), Result<()>>("remove")
                    .expect("failed to find remove gate call")
            },
            change_namespace: unsafe {
                handle
                    .dynamic_gate::<(Descriptor,), Result<()>>("change_namespace")
                    .expect("failed to find change_namespace gate call")
            },
        }
    })
}

pub type DynamicNamingHandle = NamingHandle<'static, DynamicNamerAPI>;

pub fn dynamic_naming_factory() -> Option<DynamicNamingHandle> {
    NamingHandle::new(dynamic_namer_api())
}
