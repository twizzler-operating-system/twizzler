use secgate::{util::Descriptor, SecGateReturn};
use twizzler_rt_abi::object::ObjID;

use crate::handle::NamingHandle;

// maybe this can be a macro or it's just bad design :(
pub trait NamerAPI {
    fn put(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn get(&self, desc: Descriptor) -> SecGateReturn<Option<u128>>;
    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>>;
    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn enumerate_names(&self, desc: Descriptor) -> SecGateReturn<Option<usize>>;
    fn remove(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn change_namespace(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn put_namespace(&self, desc: Descriptor) -> SecGateReturn<()>;
}
