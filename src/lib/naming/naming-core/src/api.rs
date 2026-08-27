use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

use crate::{CwdPath, GetFlags, InlinePath, NsNode, Result};

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
    fn change_root_inline(&self, desc: Descriptor, path: InlinePath) -> Result<()>;

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
    fn change_root(&self, desc: Descriptor, offset: usize, name_len: usize) -> Result<()>;

    /// This handle's working directory, derived server-side from the namespace chain. The
    /// `_inline` form reports the full length even when the path did not fit, so a caller knows
    /// to re-ask through the buffer form rather than believing a truncated path.
    fn get_cwd_inline(&self, desc: Descriptor) -> Result<CwdPath>;
    fn get_cwd(&self, desc: Descriptor, offset: usize, cap: usize) -> Result<usize>;

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

    /// Hand this handle's root and working namespace to whoever redeems the returned token.
    ///
    /// This exists because neither a path nor an ObjID can carry a working directory across a
    /// spawn intact. A path loses identity -- a rename between the spawn and the child's startup
    /// lands it somewhere else. An ObjID loses the *chain*: a namespace opened by id has no
    /// parent, so the child's `getcwd` would report `/` while it sat somewhere else entirely.
    /// The chain is live server state, so the only way to hand it over whole is to hand it over
    /// here and let the child collect it.
    ///
    /// Single-use and expiring; possession of the token is what authorises collecting it.
    fn bequeath(&self, desc: Descriptor) -> Result<u64>;
    /// Collect a bequest. A token that expired, was already collected, or never existed leaves
    /// the handle where it was -- at its root, which is where a compartment without a bequest
    /// starts anyway.
    fn redeem_bequest(&self, desc: Descriptor, token: u64) -> Result<()>;

    fn open_handle(&self) -> Result<Descriptor>;
    /// Create this handle's shared buffer on demand, returning the object to map.
    fn get_buffer(&self, desc: Descriptor) -> Result<ObjID>;
    fn close_handle(&self, desc: Descriptor) -> Result<()>;
}
