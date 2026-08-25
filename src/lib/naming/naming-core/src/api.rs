use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

use crate::{GetFlags, InlinePath, NsNode, Result};

/// The gate surface between a naming client and the server.
///
/// Every path-taking operation comes in two spellings. The `_inline` form carries its path(s) in
/// the gate arguments and touches no shared state, so any number can be in flight on one handle.
/// The buffer form is the spill path for paths longer than [`crate::INLINE_PATH_MAX`] (and the
/// reply channel for enumeration): the path sits in the handle's shared buffer at `offset`, which
/// the client allocates from its slot map — concurrent calls use disjoint slots. Two-path
/// operations pack both paths into one slot, the second at `offset + first_len`.
pub trait NamerAPI {
    // Inline forms.
    fn put_inline(&self, desc: Descriptor, path: InlinePath, id: ObjID) -> Result<()>;
    fn mkns_inline(&self, desc: Descriptor, path: InlinePath, persist: bool) -> Result<()>;
    fn link_inline(&self, desc: Descriptor, path: InlinePath, link: InlinePath) -> Result<()>;
    fn get_inline(&self, desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode>;
    fn remove_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()>;
    fn rename_inline(&self, desc: Descriptor, old: InlinePath, new: InlinePath) -> Result<()>;
    fn change_namespace_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()>;

    // Buffer (spill) forms; paths live at `offset` in the handle's shared buffer.
    fn put(&self, desc: Descriptor, offset: usize, name_len: usize, id: ObjID) -> Result<()>;
    fn mkns(&self, desc: Descriptor, offset: usize, name_len: usize, persist: bool) -> Result<()>;
    fn link(&self, desc: Descriptor, offset: usize, name_len: usize, link_len: usize)
        -> Result<()>;
    fn get(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        flags: GetFlags,
    ) -> Result<NsNode>;
    fn remove(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()>;
    fn rename(&self, desc: Descriptor, offset: usize, old_len: usize, new_len: usize)
        -> Result<()>;
    fn change_namespace(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()>;

    /// Path (possibly empty) at `offset`; the reply is written back at `offset`, at most
    /// [`crate::BUFFER_SLOT_SIZE`] bytes of `NsNode`s. Returns how many entries were written.
    fn enumerate_names(
        &self,
        desc: Descriptor,
        offset: usize,
        name_len: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize>;
    /// Like `enumerate_names`, but names the namespace by ID; `offset` is only the reply slot.
    fn enumerate_names_nsid(
        &self,
        desc: Descriptor,
        id: ObjID,
        offset: usize,
        skip: usize,
        count: usize,
    ) -> Result<usize>;

    fn open_handle(&self) -> Result<Descriptor>;
    /// Create this handle's shared buffer on demand, returning the object to map.
    fn get_buffer(&self, desc: Descriptor) -> Result<ObjID>;
    fn close_handle(&self, desc: Descriptor) -> Result<()>;
}
