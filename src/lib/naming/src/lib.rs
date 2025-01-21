#[link(name = "naming_srv")]
extern "C" {}

pub use naming_core::dynamic::*;
use naming_core::{api::NamerAPI, handle::NamingHandle};
use secgate::util::{Descriptor, HandleMgr, SimpleBuffer};
use twizzler_rt_abi::object::ObjID;

pub struct StaticNamingAPI {}

impl NamerAPI for StaticNamingAPI {
    fn put(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::put(desc)
    }

    fn get(&self, desc: Descriptor) -> secgate::SecGateReturn<Option<u128>> {
        naming_srv::get(desc)
    }

    fn open_handle(&self) -> secgate::SecGateReturn<Option<(Descriptor, ObjID)>> {
        naming_srv::open_handle()
    }

    fn close_handle(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::close_handle(desc)
    }

    fn enumerate_names(&self, desc: Descriptor) -> secgate::SecGateReturn<Option<usize>> {
        naming_srv::enumerate_names(desc)
    }

    fn remove(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::remove(desc)
    }
    
    fn change_namespace(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::change_namespace(desc)
    }
    
    fn put_namespace(&self, desc: Descriptor) -> secgate::SecGateReturn<()> {
        naming_srv::put_namespace(desc)
    }
}

static STATIC_NAMING_API: StaticNamingAPI = StaticNamingAPI {};

pub type StaticNamingHandle = NamingHandle<'static, StaticNamingAPI>;

pub fn static_naming_factory() -> Option<StaticNamingHandle> {
    NamingHandle::new(&STATIC_NAMING_API)
}
