//! Gate call points for the naming server, declared weak.
//!
//! A compartment loaded while naming-srv is present in the dynlink context binds these directly at
//! relocation time -- a call is then one trampoline invocation, no monitor traffic. A compartment
//! loaded earlier (the bootstrap chain: logboi, devmgr, pager, naming itself, init, and the
//! monitor) sees the weak imports resolve to zero; the stubs report `Unavailable`, and
//! [`crate::dynamic`] falls back to monitor-resolved dynamic gates instead.
//!
//! The gate surface itself is documented on [`crate::api::NamerAPI`]: `_inline` forms carry paths
//! by value, buffer forms carry a slot offset into the handle's shared buffer.

use secgate::util::Descriptor;
use twizzler_rt_abi::object::ObjID;

use crate::{GetFlags, InlinePath, NsNode, Result};

#[secgate::gatecall(weak)]
pub fn put_inline(desc: Descriptor, path: InlinePath, id: ObjID) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn mkns_inline(desc: Descriptor, path: InlinePath, persist: bool) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn link_inline(desc: Descriptor, path: InlinePath, link: InlinePath) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn get_inline(desc: Descriptor, path: InlinePath, flags: GetFlags) -> Result<NsNode> {}
#[secgate::gatecall(weak)]
pub fn remove_inline(desc: Descriptor, path: InlinePath) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn rename_inline(desc: Descriptor, old: InlinePath, new: InlinePath) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn change_namespace_inline(desc: Descriptor, path: InlinePath) -> Result<()> {}

#[secgate::gatecall(weak)]
pub fn put(desc: Descriptor, offset: usize, name_len: usize, id: ObjID) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn mkns(desc: Descriptor, offset: usize, name_len: usize, persist: bool) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn link(desc: Descriptor, offset: usize, name_len: usize, link_len: usize) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn get(desc: Descriptor, offset: usize, name_len: usize, flags: GetFlags) -> Result<NsNode> {}
#[secgate::gatecall(weak)]
pub fn remove(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn rename(desc: Descriptor, offset: usize, old_len: usize, new_len: usize) -> Result<()> {}
#[secgate::gatecall(weak)]
pub fn change_namespace(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {}

#[secgate::gatecall(weak)]
pub fn enumerate_names(
    desc: Descriptor,
    offset: usize,
    name_len: usize,
    skip: usize,
    count: usize,
) -> Result<usize> {
}
#[secgate::gatecall(weak)]
pub fn enumerate_names_nsid(
    desc: Descriptor,
    id: ObjID,
    offset: usize,
    skip: usize,
    count: usize,
) -> Result<usize> {
}

#[secgate::gatecall(weak)]
pub fn open_handle() -> Result<Descriptor> {}
#[secgate::gatecall(weak)]
pub fn get_buffer(desc: Descriptor) -> Result<ObjID> {}
#[secgate::gatecall(weak)]
pub fn close_handle(desc: Descriptor) -> Result<()> {}

#[secgate::gatecall(weak)]
pub fn namer_start(bootstrap: ObjID) -> Result<ObjID> {}

/// Whether this compartment's weak gate bindings resolved at load time. All the imports resolve in
/// the same relocation pass, so probing one answers for the set.
pub fn bound() -> bool {
    __twz_secgate_impl_open_handle_mod::weak_addr().is_some()
}

#[cfg(test)]
mod tests {
    /// No server exports this gate, so its weak import must relocate to zero. Guards the
    /// counterpart failure of the test below: a relocator writing nonzero garbage for an
    /// unresolved weak symbol would make `bound()` pass without any binding having happened.
    #[secgate::gatecall(weak)]
    fn naming_weak_canary_unresolvable() -> crate::Result<()> {}

    /// Positive control for the weak gate binding. This test binary is loaded after naming-srv,
    /// so its weak imports must have resolved to the trampolines. A green boot alone cannot
    /// distinguish "weak binding works" from "every call silently fell back to dynamic gates";
    /// this can.
    #[test]
    fn weak_gates_bound() {
        assert!(super::bound());
        assert!(__twz_secgate_impl_naming_weak_canary_unresolvable_mod::weak_addr().is_none());
    }

    /// Concurrency canary for the single shared runtime handle: every `std::fs` path op in this
    /// process goes through one `NamingHandle` now, inline gates for short paths and buffer slots
    /// for enumeration. Threads hammering lookups and readdir concurrently is exactly the load the
    /// old per-call-exclusive handle pool existed to serve; this asserts the shared handle serves
    /// it correctly (same answers under contention), not just without crashing.
    #[test]
    fn concurrent_shared_handle() {
        let expected = std::fs::read_dir("/initrd").unwrap().count();
        assert!(expected > 0);
        let threads: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        assert!(std::fs::metadata("/initrd").is_ok());
                        assert_eq!(std::fs::read_dir("/initrd").unwrap().count(), expected);
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
    }
}
