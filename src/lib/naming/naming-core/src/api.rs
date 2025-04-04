use secgate::{util::Descriptor, SecGateReturn};
use twizzler_rt_abi::object::ObjID;

use crate::{GetFlags, NsNode, Result};

// maybe this can be a macro or it's just bad design :(
pub trait NamerAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> SecGateReturn<Result<()>>;
    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> SecGateReturn<Result<()>>;
    fn link(
        &self,
        desc: Descriptor,
        name_len: usize,
        link_name: usize,
    ) -> SecGateReturn<Result<()>>;
    fn get(
        &self,
        desc: Descriptor,
        name_len: usize,
        flags: GetFlags,
    ) -> SecGateReturn<Result<NsNode>>;
    fn open_handle(&self) -> SecGateReturn<Option<(Descriptor, ObjID)>>;
    fn close_handle(&self, desc: Descriptor) -> SecGateReturn<()>;
    fn enumerate_names(&self, desc: Descriptor, name_len: usize) -> SecGateReturn<Result<usize>>;
    fn enumerate_names_nsid(&self, desc: Descriptor, id: ObjID) -> SecGateReturn<Result<usize>>;
    fn remove(&self, desc: Descriptor, name_len: usize) -> SecGateReturn<Result<()>>;
    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> SecGateReturn<Result<()>>;
}
