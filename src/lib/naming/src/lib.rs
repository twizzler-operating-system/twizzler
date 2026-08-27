use naming_core::{api::NamerAPI, gates, handle::NamingHandle, CwdPath, InlinePath, Result};
pub use naming_core::{dynamic::*, gates::namer_start, GetFlags, NsNode, NsNodeKind};
use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

/// Calls the naming gates through their weak bindings (see `naming_core::gates`): direct
/// trampoline calls in a compartment loaded after naming-srv, `Unavailable` in one loaded before.
/// A caller that might run before the namer wants `dynamic_naming_factory` instead, which falls
/// back to monitor-resolved gates.
pub struct StaticNamingAPI {}

impl NamerAPI for StaticNamingAPI {
    fn put_inline(&self, desc: Descriptor, path: InlinePath, id: ObjID) -> Result<()> {
        gates::put_inline(desc, path, id)
    }

    fn mkns_inline(&self, desc: Descriptor, path: InlinePath, persist: bool) -> Result<()> {
        gates::mkns_inline(desc, path, persist)
    }

    fn link_inline(&self, desc: Descriptor, path: InlinePath, link: InlinePath) -> Result<()> {
        gates::link_inline(desc, path, link)
    }

    fn get_inline(&self, desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {
        gates::get_inline(desc, path, flags)
    }

    fn remove_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        gates::remove_inline(desc, path)
    }

    fn rename_inline(&self, desc: Descriptor, old: InlinePath, new: InlinePath) -> Result<()> {
        gates::rename_inline(desc, old, new)
    }

    fn change_namespace_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        gates::change_namespace_inline(desc, path)
    }

    fn change_root_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()> {
        gates::change_root_inline(desc, path)
    }

    fn put(&self, desc: Descriptor, offset: usize, name_len: usize, id: ObjID) -> Result<()> {
        gates::put(desc, offset, name_len, id)
    }

    fn mkns(&self, desc: Descriptor, offset: usize, name_len: usize, persist: bool) -> Result<()> {
        gates::mkns(desc, offset, name_len, persist)
    }

    fn link(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        link_len: usize,
    ) -> Result<()> {
        gates::link(desc, offset, name_len, link_len)
    }

    fn get(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        flags: GetFlags,
    ) -> Result<NsNode> {
        gates::get(desc, offset, name_len, flags)
    }

    fn remove(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        gates::remove(desc, offset, name_len)
    }

    fn rename(
        &self,
        desc: Descriptor,
        offset: usize,
        old_len: usize,
        new_len: usize,
    ) -> Result<()> {
        gates::rename(desc, offset, old_len, new_len)
    }

    fn change_namespace(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        gates::change_namespace(desc, offset, name_len)
    }

    fn change_root(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {
        gates::change_root(desc, offset, name_len)
    }

    fn get_cwd_inline(&self, desc: Descriptor) -> Result<CwdPath> {
        gates::get_cwd_inline(desc)
    }

    fn get_cwd(&self, desc: Descriptor, offset: usize, cap: usize) -> Result<usize> {
        gates::get_cwd(desc, offset, cap)
    }

    fn enumerate_names(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        gates::enumerate_names(desc, offset, name_len, skip, count)
    }

    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
        offset: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize> {
        gates::enumerate_names_nsid(desc, id, offset, skip, count)
    }

    fn bequeath(&self, desc: Descriptor) -> Result<u64> {
        gates::bequeath(desc)
    }

    fn redeem_bequest(&self, desc: Descriptor, token: u64) -> Result<()> {
        gates::redeem_bequest(desc, token)
    }

    fn open_handle(&self) -> Result<Descriptor> {
        gates::open_handle()
    }

    fn get_buffer(&self, desc: Descriptor) -> Result<ObjID> {
        gates::get_buffer(desc)
    }

    fn close_handle(&self, desc: Descriptor) -> Result<()> {
        gates::close_handle(desc)
    }
}

static STATIC_NAMING_API: StaticNamingAPI = StaticNamingAPI {};

pub type StaticNamingHandle = NamingHandle<'static, StaticNamingAPI>;

pub fn static_naming_factory() -> Option<StaticNamingHandle> {
    NamingHandle::new(&STATIC_NAMING_API)
}
