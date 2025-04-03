use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::{util::Descriptor, DynamicSecGate};
use twizzler_rt_abi::object::ObjID;

use crate::{api::NamerAPI, handle::NamingHandle, GetFlags, NsNode, Result};

pub struct DynamicNamerAPI {
    _handle: &'static CompartmentHandle,
    put: DynamicSecGate<'static, (Descriptor, usize, ObjID), ()>,
    mkns: DynamicSecGate<'static, (Descriptor, usize, bool), ()>,
    link: DynamicSecGate<'static, (Descriptor, usize, usize), ()>,
    get: DynamicSecGate<'static, (Descriptor, usize, GetFlags), NsNode>,
    open_handle: DynamicSecGate<'static, (), (Descriptor, ObjID)>,
    close_handle: DynamicSecGate<'static, (Descriptor,), ()>,
    enumerate_names: DynamicSecGate<'static, (Descriptor, usize), usize>,
    enumerate_names_nsid: DynamicSecGate<'static, (Descriptor, ObjID), usize>,
    remove: DynamicSecGate<'static, (Descriptor, usize), ()>,
    change_namespace: DynamicSecGate<'static, (Descriptor, usize), ()>,
}

impl NamerAPI for DynamicNamerAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> Result<()> {
        (self.put)(desc, name_len, id)
    }

    fn get(&self, desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {
        (self.get)(desc, name_len, flags)
    }

    fn open_handle(&self) -> Result<(Descriptor, ObjID)> {
        (self.open_handle)()
    }

    fn close_handle(&self, desc: Descriptor) -> () {
        let _ = (self.close_handle)(desc);
    }

    fn enumerate_names(&self, desc: Descriptor, name_len: usize) -> Result<usize> {
        (self.enumerate_names)(desc, name_len)
    }

    fn enumerate_names_nsid(&self, desc: Descriptor, id: ObjID) -> Result<usize> {
        (self.enumerate_names_nsid)(desc, id)
    }

    fn remove(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        (self.remove)(desc, name_len)
    }

    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        (self.change_namespace)(desc, name_len)
    }

    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {
        (self.mkns)(desc, name_len, persist)
    }

    fn link(&self, desc: Descriptor, name_len: usize, link_name: usize) -> Result<()> {
        (self.link)(desc, name_len, link_name)
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
                    .dynamic_gate("put")
                    .expect("failed to find put gate call")
            },
            mkns: unsafe {
                handle
                    .dynamic_gate("mkns")
                    .expect("failed to find put gate call")
            },
            link: unsafe {
                handle
                    .dynamic_gate("link")
                    .expect("failed to find put gate call")
            },
            get: unsafe {
                handle
                    .dynamic_gate("get")
                    .expect("failed to find get gate call")
            },
            open_handle: unsafe {
                handle
                    .dynamic_gate::<(), (Descriptor, ObjID)>("open_handle")
                    .expect("failed to find open_handle gate call")
            },
            close_handle: unsafe {
                handle
                    .dynamic_gate::<(Descriptor,), ()>("close_handle")
                    .expect("failed to find close_handle gate call")
            },
            enumerate_names: unsafe {
                handle
                    .dynamic_gate("enumerate_names")
                    .expect("failed to find enumerate_names gate call")
            },
            enumerate_names_nsid: unsafe {
                handle
                    .dynamic_gate("enumerate_names_nsid")
                    .expect("failed to find enumerate_names gate call")
            },
            remove: unsafe {
                handle
                    .dynamic_gate("remove")
                    .expect("failed to find remove gate call")
            },
            change_namespace: unsafe {
                handle
                    .dynamic_gate("change_namespace")
                    .expect("failed to find change_namespace gate call")
            },
        }
    })
}

pub type DynamicNamingHandle = NamingHandle<'static, DynamicNamerAPI>;

pub fn dynamic_naming_factory() -> Option<DynamicNamingHandle> {
    NamingHandle::new(dynamic_namer_api())
}
