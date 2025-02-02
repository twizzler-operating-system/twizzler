#[link(name = "naming_srv")]
extern "C" {}

use naming_core::{api::NamerAPI, handle::NamingHandle, Result};
pub use naming_core::{dynamic::*, Entry, EntryType};
use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

pub struct StaticNamingAPI {}

impl NamerAPI for StaticNamingAPI {
    fn put(&self, desc: Descriptor) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::put(desc)
    }

    fn get(&self, desc: Descriptor) -> secgate::SecGateReturn<Result<Entry>> {
        naming_srv::get(desc)
    }

    fn open_handle(&self) -> secgate::SecGateReturn<Option<(Descriptor, ObjID)>> {
        naming_srv::open_handle()
    }

    fn close_handle(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::close_handle(desc)
    }

    fn enumerate_names(&self, desc: Descriptor) -> secgate::SecGateReturn<Result<usize>> {
        naming_srv::enumerate_names(desc)
    }

    fn remove(&self, desc: Descriptor, recursive: bool) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::remove(desc, recursive)
    }

    fn change_namespace(&self, desc: Descriptor) -> secgate::SecGateReturn<Result<()>> {
        naming_srv::change_namespace(desc)
    }
}

static STATIC_NAMING_API: StaticNamingAPI = StaticNamingAPI {};

pub type StaticNamingHandle = NamingHandle<'static, StaticNamingAPI>;

pub fn static_naming_factory() -> Option<StaticNamingHandle> {
    NamingHandle::new(&STATIC_NAMING_API)
}
