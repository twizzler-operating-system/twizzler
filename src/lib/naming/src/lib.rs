#[link(name = "naming_srv")]
extern "C" {}

use naming_core::{api::NamerAPI, handle::NamingHandle, Result};
pub use naming_core::{dynamic::*, GetFlags, NsNode, NsNodeKind};
use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

pub struct StaticNamingAPI {}

impl NamerAPI for StaticNamingAPI {
    fn put(
        &self,
        desc: Descriptor,
        name_len: usize,
        id: ObjID,
    ) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::put(desc, name_len, id)
    }

    fn get(
        &self,
        desc: Descriptor,
        name_len: usize,
        flags: GetFlags,
    ) -> secgate::SecGateReturn<Result<NsNode>> {
        naming_srv::get(desc, name_len, flags)
    }

    fn open_handle(&self) -> secgate::SecGateReturn<Option<(Descriptor, ObjID)>> {
        naming_srv::open_handle()
    }

    fn close_handle(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::close_handle(desc)
    }

    fn enumerate_names(
        &self,
        desc: Descriptor,
        name_len: usize,
    ) -> secgate::SecGateReturn<Result<usize>> {
        naming_srv::enumerate_names(desc, name_len)
    }

    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
    ) -> secgate::SecGateReturn<Result<usize>> {
        naming_srv::enumerate_names_nsid(desc, id)
    }

    fn remove(&self, desc: Descriptor, name_len: usize) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::remove(desc, name_len)
    }

    fn change_namespace(
        &self,
        desc: Descriptor,
        name_len: usize,
    ) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::change_namespace(desc, name_len)
    }

    fn mkns(
        &self,
        desc: Descriptor,
        name_len: usize,
        persist: bool,
    ) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::mkns(desc, name_len, persist)
    }

    fn link(
        &self,
        desc: Descriptor,
        name_len: usize,
        link_len: usize,
    ) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::link(desc, name_len, link_len)
    }
}

static STATIC_NAMING_API: StaticNamingAPI = StaticNamingAPI {};

pub type StaticNamingHandle = NamingHandle<'static, StaticNamingAPI>;

pub fn static_naming_factory() -> Option<StaticNamingHandle> {
    NamingHandle::new(&STATIC_NAMING_API)
}
