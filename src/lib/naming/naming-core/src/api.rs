use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

use crate::{GetFlags, InlinePath, NsNode, Result};

// maybe this can be a macro or it's just bad design :(
pub trait NamerAPI {
    fn put(&self, desc: Descriptor, name_len: usize, id: ObjID) -> Result<()>;
    fn mkns(&self, desc: Descriptor, name_len: usize, persist: bool) -> Result<()>;
    fn link(&self, desc: Descriptor, name_len: usize, link_name: usize) -> Result<()>;
    fn get(&self, desc: Descriptor, name_len: usize, flags: GetFlags) -> Result<NsNode>;
    /// Lookup carrying its path in the arguments, so it needs no shared buffer.
    fn get_inline(&self, desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode>;
    fn open_handle(&self) -> Result<Descriptor>;
    /// Create this handle's shared buffer on demand, returning the object to map.
    fn get_buffer(&self, desc: Descriptor) -> Result<ObjID>;
    fn close_handle(&self, desc: Descriptor) -> Result<()>;
    fn enumerate_names(
        &self,
        desc: Descriptor,
        name_len: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize>;
    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
        skip: usize,
        count: usize,
    ) -> Result<usize>;
    fn remove(&self, desc: Descriptor, name_len: usize) -> Result<()>;
    fn rename(&self, desc: Descriptor, old_len: usize, new_len: usize) -> Result<()>;
    fn change_namespace(&self, desc: Descriptor, name_len: usize) -> Result<()>;
}
