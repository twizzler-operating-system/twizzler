use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::{util::Descriptor, DynamicSecGate};
use twizzler_rt_abi::object::ObjID;

use crate::{api::NamerAPI, handle::NamingHandle, GetFlags, InlinePath, NsNode, Result};

/// Gate addresses are resolved on first use, not all at once.
///
/// Every field here is a `dynamic_gate` resolution, and building them eagerly cost one gate call
/// each the moment any naming was done. Measured over a test boot, gate resolution was 49% of all
/// per-thread-buffer traffic in the monitor at ~14 resolutions per compartment loaded -- and most
/// compartments only ever call `get`/`get_inline`/`open_handle`. `OnceLock` per gate turns that
/// into "pay for what you use".
///
/// A missing gate now panics at first use rather than at init. That is a deliberate trade: a gate
/// nobody calls no longer takes the compartment down.
macro_rules! lazy_gates {
    ($($field:ident : $ty:ty = $name:literal),* $(,)?) => {
        pub struct DynamicNamerAPI {
            handle: &'static CompartmentHandle,
            $( $field: OnceLock<$ty>, )*
        }

        impl DynamicNamerAPI {
            fn new(handle: &'static CompartmentHandle) -> Self {
                Self { handle, $( $field: OnceLock::new(), )* }
            }
            $(
                fn $field(&self) -> &$ty {
                    self.$field.get_or_init(|| unsafe {
                        self.handle
                            .dynamic_gate($name)
                            .expect(concat!("failed to find ", $name, " gate call"))
                    })
                }
            )*
        }
    };
}

lazy_gates! {
    put: DynamicSecGate<'static, (Descriptor, usize, ObjID), ()> = "put",
    mkns: DynamicSecGate<'static, (Descriptor, usize, bool), ()> = "mkns",
    link: DynamicSecGate<'static, (Descriptor, usize, usize), ()> = "link",
    get: DynamicSecGate<'static, (Descriptor, usize, GetFlags), NsNode> = "get",
    get_inline: DynamicSecGate<'static, (Descriptor, InlinePath, GetFlags), NsNode> = "get_inline",
    open_handle: DynamicSecGate<'static, (), Descriptor> = "open_handle",
    get_buffer: DynamicSecGate<'static, (Descriptor,), ObjID> = "get_buffer",
    close_handle: DynamicSecGate<'static, (Descriptor,), ()> = "close_handle",
    enumerate_names: DynamicSecGate<'static, (Descriptor, usize, usize, usize), usize>
        = "enumerate_names",
    enumerate_names_nsid: DynamicSecGate<'static, (Descriptor, ObjID, usize, usize), usize>
        = "enumerate_names_nsid",
    remove: DynamicSecGate<'static, (Descriptor, usize), ()> = "remove",
    rename: DynamicSecGate<'static, (Descriptor, usize, usize), ()> = "rename",
    change_namespace: DynamicSecGate<'static, (Descriptor, usize), ()> = "change_namespace",
}

impl NamerAPI for DynamicNamerAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> Result<()> {
        (self.put())(desc, name_len, id)
    }

    fn get(&self, desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {
        (self.get())(desc, name_len, flags)
    }

    fn get_inline(&self, desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
        (self.get_inline())(desc, path, flags)
    }

    fn open_handle(&self) -> Result<Descriptor> {
        (self.open_handle())()
    }

    fn get_buffer(&self, desc: Descriptor) -> Result<ObjID> {
        (self.get_buffer())(desc)
    }

    fn close_handle(&self, desc: Descriptor) -> Result<()> {
        let _ = (self.close_handle())(desc);
        Ok(())
    }

    fn enumerate_names(
        &self,
        desc: Descriptor,
        name_len: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        (self.enumerate_names())(desc, name_len, skip, count)
    }

    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        (self.enumerate_names_nsid())(desc, id, skip, count)
    }

    fn remove(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        (self.remove())(desc, name_len)
    }

    fn rename(&self, desc: Descriptor, old_len: usize, new_len: usize) -> Result<()> {
        (self.rename())(desc, old_len, new_len)
    }

    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        (self.change_namespace())(desc, name_len)
    }

    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {
        (self.mkns())(desc, name_len, persist)
    }

    fn link(&self, desc: Descriptor, name_len: usize, link_name: usize) -> Result<()> {
        (self.link())(desc, name_len, link_name)
    }
}

static DYNAMIC_NAMER_API: OnceLock<DynamicNamerAPI> = OnceLock::new();

pub fn dynamic_namer_api() -> &'static DynamicNamerAPI {
    DYNAMIC_NAMER_API.get_or_init(|| {
        let handle = Box::leak(Box::new(
            CompartmentHandle::lookup("naming").expect("failed to open namer compartment"),
        ));
        DynamicNamerAPI::new(handle)
    })
}

pub type DynamicNamingHandle = NamingHandle<'static, DynamicNamerAPI>;

pub fn dynamic_naming_factory() -> Option<DynamicNamingHandle> {
    NamingHandle::new(dynamic_namer_api())
}
