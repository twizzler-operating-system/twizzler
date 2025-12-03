use naming_core::{api::NamerAPI, handle::NamingHandle, Result};
pub use naming_core::{dynamic::*, GetFlags, NsNode, NsNodeKind};
use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

pub struct StaticNamingAPI {}

#[secgate::gatecall]
fn put(desc: Descriptor, name_len: usize, id: ObjID) -> Result<()> {}
#[secgate::gatecall]
fn get(desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {}
#[secgate::gatecall]
fn open_handle() -> Result<(Descriptor, ObjID)> {}
#[secgate::gatecall]
fn close_handle(desc: Descriptor) -> Result<()> {}
#[secgate::gatecall]
fn enumerate_names(desc: Descriptor, name_len: usize) -> Result<usize> {}
#[secgate::gatecall]
fn enumerate_names_nsid(desc: Descriptor, id: ObjID) -> Result<usize> {}
#[secgate::gatecall]
fn remove(desc: Descriptor, name_len: usize) -> Result<()> {}
#[secgate::gatecall]
fn change_namespace(desc: Descriptor, name_len: usize) -> Result<()> {}
#[secgate::gatecall]
fn mkns(desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {}
#[secgate::gatecall]
fn link(desc: Descriptor, name_len: usize, link_len: usize) -> Result<()> {}

#[secgate::gatecall]
pub fn namer_start(bootstrap: ObjID) -> Result<ObjID> {}

impl NamerAPI for StaticNamingAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> Result<()> {
        put(desc, name_len, id)
    }

    fn get(&self, desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode> {
        get(desc, name_len, flags)
    }

    fn open_handle(&self) -> Result<(Descriptor, ObjID)> {
        open_handle()
    }

    fn close_handle(&self, desc: Descriptor) -> Result<()> {
        close_handle(desc)
    }

    fn enumerate_names(&self, desc: Descriptor, name_len: usize) -> Result<usize> {
        enumerate_names(desc, name_len)
    }

    fn enumerate_names_nsid(&self, desc: Descriptor, id: ObjID) -> Result<usize> {
        enumerate_names_nsid(desc, id)
    }

    fn remove(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        remove(desc, name_len)
    }

    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> Result<()> {
        change_namespace(desc, name_len)
    }

    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> Result<()> {
        mkns(desc, name_len, persist)
    }

    fn link(&self, desc: Descriptor, name_len: usize, link_len: usize) -> Result<()> {
        link(desc, name_len, link_len)
    }
}

static STATIC_NAMING_API: StaticNamingAPI = StaticNamingAPI {};

pub type StaticNamingHandle = NamingHandle<'static, StaticNamingAPI>;

pub fn static_naming_factory() -> Option<StaticNamingHandle> {
    NamingHandle::new(&STATIC_NAMING_API)
}
