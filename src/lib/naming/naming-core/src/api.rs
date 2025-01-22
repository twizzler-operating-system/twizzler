use secgate::{util::Descriptor, SecGateReturn};
use twizzler_rt_abi::object::ObjID;

pub trait NamerAPI {
    fn put(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn get(&self, desc: Descriptor) -> SecGateReturn<Option<u128>>;
    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>>;
    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn enumerate_names(&self, desc: Descriptor) -> SecGateReturn<Option<usize>>;
    fn remove(&self, desc: Descriptor) -> SecGateReturn<()>;
}
