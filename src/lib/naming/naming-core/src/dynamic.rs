use std::sync::OnceLock;

use monitor_api::CompartmentHandle;
use secgate::{util::Descriptor, DynamicSecGate};
use twizzler_rt_abi::object::ObjID;

use crate::{api::NamerAPI, handle::NamingHandle, CwdPath, GetFlags, InlinePath, NsNode, Result};

/// Gate addresses are resolved on first use, not all at once -- and weakly-bound gates never
/// touch the monitor at all.
///
/// Every field resolves in two steps. First it consults the weak import in [`crate::gates`]: a
/// compartment loaded while naming-srv was present had those bound directly at relocation, so the
/// address is already in hand. Only when the weak binding is absent (this compartment loaded
/// before naming-srv -- the bootstrap chain) does it fall back to a monitor `dynamic_gate`
/// resolution. Measured before the weak path existed, gate resolution was 49% of all
/// per-thread-buffer traffic in the monitor at ~14 resolutions per compartment loaded; `OnceLock`
/// per gate already turned that into "pay for what you use", and the weak binding turns the common
/// case into "pay nothing".
///
/// A missing gate panics at first use rather than at init. That is a deliberate trade: a gate
/// nobody calls no longer takes the compartment down.
macro_rules! lazy_gates {
    ($($field:ident / $gmod:ident : $ty:ty = $name:literal),* $(,)?) => {
        pub struct DynamicNamerAPI {
            handle: OnceLock<&'static CompartmentHandle>,
            $( $field: OnceLock<$ty>, )*
        }

        impl DynamicNamerAPI {
            fn new() -> Self {
                Self { handle: OnceLock::new(), $( $field: OnceLock::new(), )* }
            }
            /// Only reached on the fallback path, i.e. when this compartment loaded before
            /// naming-srv.
            fn handle(&self) -> &'static CompartmentHandle {
                self.handle.get_or_init(|| {
                    Box::leak(Box::new(
                        CompartmentHandle::lookup("naming")
                            .expect("failed to open namer compartment"),
                    ))
                })
            }
            $(
                fn $field(&self) -> &$ty {
                    self.$field.get_or_init(|| {
                        if let Some(addr) = crate::gates::$gmod::weak_addr() {
                            return unsafe { DynamicSecGate::new(addr) };
                        }
                        unsafe {
                            self.handle()
                                .dynamic_gate($name)
                                .expect(concat!("failed to find ", $name, " gate call"))
                        }
                    })
                }
            )*
        }
    };
}

lazy_gates! {
    put_inline / __twz_secgate_impl_put_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath, ObjID), ()> = "put_inline",
    mkns_inline / __twz_secgate_impl_mkns_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath, bool), ()> = "mkns_inline",
    link_inline / __twz_secgate_impl_link_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath, InlinePath), ()> = "link_inline",
    get_inline / __twz_secgate_impl_get_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath, GetFlags), NsNode> = "get_inline",
    remove_inline / __twz_secgate_impl_remove_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath), ()> = "remove_inline",
    rename_inline / __twz_secgate_impl_rename_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath, InlinePath), ()> = "rename_inline",
    change_namespace_inline / __twz_secgate_impl_change_namespace_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath), ()> = "change_namespace_inline",
    change_root_inline / __twz_secgate_impl_change_root_inline_mod:
        DynamicSecGate<'static, (Descriptor, InlinePath), ()> = "change_root_inline",
    put / __twz_secgate_impl_put_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, ObjID), ()> = "put",
    mkns / __twz_secgate_impl_mkns_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, bool), ()> = "mkns",
    link / __twz_secgate_impl_link_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, usize), ()> = "link",
    get / __twz_secgate_impl_get_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, GetFlags), NsNode> = "get",
    remove / __twz_secgate_impl_remove_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize), ()> = "remove",
    rename / __twz_secgate_impl_rename_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, usize), ()> = "rename",
    change_namespace / __twz_secgate_impl_change_namespace_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize), ()> = "change_namespace",
    change_root / __twz_secgate_impl_change_root_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize), ()> = "change_root",
    get_cwd_inline / __twz_secgate_impl_get_cwd_inline_mod:
        DynamicSecGate<'static, (Descriptor,), CwdPath> = "get_cwd_inline",
    get_cwd / __twz_secgate_impl_get_cwd_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize), usize> = "get_cwd",
    enumerate_names / __twz_secgate_impl_enumerate_names_mod:
        DynamicSecGate<'static, (Descriptor, usize, usize, usize, usize), usize>
        = "enumerate_names",
    enumerate_names_nsid / __twz_secgate_impl_enumerate_names_nsid_mod:
        DynamicSecGate<'static, (Descriptor, ObjID, usize, usize, usize), usize>
        = "enumerate_names_nsid",
    bequeath / __twz_secgate_impl_bequeath_mod:
        DynamicSecGate<'static, (Descriptor,), u64> = "bequeath",
    redeem_bequest / __twz_secgate_impl_redeem_bequest_mod:
        DynamicSecGate<'static, (Descriptor, u64), ()> = "redeem_bequest",
    open_handle / __twz_secgate_impl_open_handle_mod:
        DynamicSecGate<'static, (), Descriptor> = "open_handle",
    get_buffer / __twz_secgate_impl_get_buffer_mod:
        DynamicSecGate<'static, (Descriptor,), ObjID> = "get_buffer",
    close_handle / __twz_secgate_impl_close_handle_mod:
        DynamicSecGate<'static, (Descriptor,), ()> = "close_handle",
}

impl NamerAPI for DynamicNamerAPI {
    fn put_inline(&self, desc: Descriptor, path: InlinePath, id: ObjID) -> Result<()> {
        (self.put_inline())(desc, path, id)
    }

    fn mkns_inline(&self, desc: Descriptor, path: InlinePath, persist: bool) -> Result<()> {
        (self.mkns_inline())(desc, path, persist)
    }

    fn link_inline(&self, desc: Descriptor, path: InlinePath, link: InlinePath) -> Result<()> {
        (self.link_inline())(desc, path, link)
    }

    fn get_inline(&self, desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
        (self.get_inline())(desc, path, flags)
    }

    fn remove_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        (self.remove_inline())(desc, path)
    }

    fn rename_inline(&self, desc: Descriptor, old: InlinePath, new: InlinePath) -> Result<()> {
        (self.rename_inline())(desc, old, new)
    }

    fn change_namespace_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        (self.change_namespace_inline())(desc, path)
    }

    fn change_root_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        (self.change_root_inline())(desc, path)
    }

    fn put(&self, desc: Descriptor, offset: usize, name_len: usize, id: ObjID) -> Result<()> {
        (self.put())(desc, offset, name_len, id)
    }

    fn mkns(&self, desc: Descriptor, offset: usize, name_len: usize, persist: bool) -> Result<()> {
        (self.mkns())(desc, offset, name_len, persist)
    }

    fn link(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        link_len: usize,
    ) -> Result<()> {
        (self.link())(desc, offset, name_len, link_len)
    }

    fn get(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        flags: GetFlags,
    ) -> Result<NsNode> {
        (self.get())(desc, offset, name_len, flags)
    }

    fn remove(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        (self.remove())(desc, offset, name_len)
    }

    fn rename(
        &self,
        desc: Descriptor,
        offset: usize,
        old_len: usize,
        new_len: usize,
    ) -> Result<()> {
        (self.rename())(desc, offset, old_len, new_len)
    }

    fn change_namespace(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        (self.change_namespace())(desc, offset, name_len)
    }

    fn change_root(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        (self.change_root())(desc, offset, name_len)
    }

    fn get_cwd_inline(&self, desc: Descriptor) -> Result<CwdPath> {
        (self.get_cwd_inline())(desc)
    }

    fn get_cwd(&self, desc: Descriptor, offset: usize, cap: usize) -> Result<usize> {
        (self.get_cwd())(desc, offset, cap)
    }

    fn enumerate_names(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        (self.enumerate_names())(desc, offset, name_len, skip, count)
    }

    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
        offset: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        (self.enumerate_names_nsid())(desc, id, offset, skip, count)
    }

    fn bequeath(&self, desc: Descriptor) -> Result<u64> {
        (self.bequeath())(desc)
    }

    fn redeem_bequest(&self, desc: Descriptor, token: u64) -> Result<()> {
        (self.redeem_bequest())(desc, token)
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
}

static DYNAMIC_NAMER_API: OnceLock<DynamicNamerAPI> = OnceLock::new();

/// Never panics anymore: the compartment lookup moved into the per-gate fallback, so a
/// weakly-bound compartment builds this without any monitor traffic at all.
pub fn dynamic_namer_api() -> &'static DynamicNamerAPI {
    DYNAMIC_NAMER_API.get_or_init(DynamicNamerAPI::new)
}

pub type DynamicNamingHandle = NamingHandle<'static, DynamicNamerAPI>;

pub fn dynamic_naming_factory() -> Option<DynamicNamingHandle> {
    NamingHandle::new(dynamic_namer_api())
}
