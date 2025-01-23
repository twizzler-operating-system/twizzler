use secgate::{util::Descriptor, SecGateReturn};
use twizzler_rt_abi::object::ObjID;

use crate::{Entry, Result};

// maybe this can be a macro or it's just bad design :(
pub trait NamerAPI {
    fn put(&self, desc: Descriptor) -> SecGateReturn<Result<()>>;
    fn get(&self, desc: Descriptor) -> SecGateReturn<Result<Entry>>;
    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>>;
    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn enumerate_names(&self, desc: Descriptor) -> SecGateReturn<Result<usize>>;
    fn remove(&self, desc: Descriptor) -> SecGateReturn<Result<()>>;
    fn change_namespace(&self, desc: Descriptor) -> SecGateReturn<Result<()>>;
}
