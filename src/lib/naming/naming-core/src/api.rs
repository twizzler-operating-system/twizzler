use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

use crate::{GetFlags, NsNode, Result};

// maybe this can be a macro or it's just bad design :(
pub trait NamerAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> Result<()>;
    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> Result<()>;
    fn link(&self, desc: Descriptor, name_len: usize, link_name: usize) -> Result<()>;
    fn get(&self, desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode>;
    fn open_handle(&self) -> Result<(Descriptor, ObjID)>;
    fn close_handle(&self, desc: Descriptor) -> ();
    fn enumerate_names(&self, desc: Descriptor, name_len: usize) -> Result<usize>;
    fn enumerate_names_nsid(&self, desc: Descriptor, id: ObjID) -> Result<usize>;
    fn remove(&self, desc: Descriptor, name_len: usize) -> Result<()>;
    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> Result<()>;
}
