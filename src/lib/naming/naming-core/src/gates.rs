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

use crate::{CwdPath, GetFlags, InlinePath, NsNode, Result};

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
pub fn change_root_inline(desc: Descriptor, path: InlinePath) -> Result<()> {}

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
pub fn change_root(desc: Descriptor, offset: usize, name_len: usize) -> Result<()> {}

/// This handle's working directory. The server derives it from the namespace chain per call --
/// neither side keeps a copy that could disagree with where the handle actually is.
#[secgate::gatecall(weak)]
pub fn get_cwd_inline(desc: Descriptor) -> Result<CwdPath> {}
/// Spill form of [`get_cwd_inline`], for a cwd longer than an inline reply holds.
#[secgate::gatecall(weak)]
pub fn get_cwd(desc: Descriptor, offset: usize, cap: usize) -> Result<usize> {}

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

/// Mint a single-use token carrying this handle's root and working namespace, for a compartment
/// this one is about to spawn. See [`crate::api::NamerAPI::bequeath`].
#[secgate::gatecall(weak)]
pub fn bequeath(desc: Descriptor) -> Result<u64> {}
/// Adopt a bequeathed root and working namespace onto this handle.
#[secgate::gatecall(weak)]
pub fn redeem_bequest(desc: Descriptor, token: u64) -> Result<()> {}

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

    /// Where the working directory lives: the naming server, on this compartment's handle.
    ///
    /// Every assertion here is written to fail if the runtime keeps its own idea of the cwd
    /// alongside the server's. The last section is the one that could only have passed by
    /// accident before: it sets a nameroot that is *not* the working directory, then checks the
    /// working directory against the server -- by listing "." and comparing that to a listing of
    /// the path `current_dir()` reports -- rather than against the runtime's own record, which is
    /// the thing under test.
    ///
    /// One test rather than four: the harness runs tests on parallel threads and the working
    /// directory is process-wide, so these have to be sequential with respect to each other.
    #[test]
    fn cwd_is_server_state() {
        use std::path::Path;

        fn listing(path: impl AsRef<Path>) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(path)
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        let restore = std::env::current_dir().unwrap();

        // A move is visible to both the reporter and the resolver, because they are the same
        // state: `current_dir()` and a relative lookup answer out of one working namespace.
        std::env::set_current_dir("/initrd").unwrap();
        assert_eq!(std::env::current_dir().unwrap(), Path::new("/initrd"));
        assert!(!listing(".").is_empty());
        assert_eq!(listing("."), listing("/initrd"));

        // `..` walks the same chain the reported path is derived from.
        std::env::set_current_dir("..").unwrap();
        assert_eq!(std::env::current_dir().unwrap(), Path::new("/"));
        assert_eq!(listing("."), listing("/"));

        // `..` at the root is the root, not an error: the walk clamps there instead of falling
        // through to a ".." entry that the root has none of.
        std::env::set_current_dir("..").unwrap();
        assert_eq!(std::env::current_dir().unwrap(), Path::new("/"));

        // Setting a nameroot that is not the working directory must not move the working
        // directory. The runtime used to call `change_namespace` for every root, which moved the
        // server while leaving its own `Current` entry behind -- reporting "/" while resolving
        // relative paths in /initrd. Checked against the server, not against the report.
        let before = listing(".");
        // Whether the runtime lets this root be set is beside the point. What must hold is that
        // asking about a root which is *not* the working directory cannot move the working
        // directory -- and it used to, because every root went through `change_namespace`.
        let _ = twizzler_rt_abi::fd::twz_rt_set_nameroot(
            twizzler_rt_abi::fd::NameRoot::Home,
            b"/initrd",
        );
        let reported = std::env::current_dir().unwrap();
        assert_eq!(reported, Path::new("/"));
        assert_eq!(listing("."), before);
        assert_eq!(listing("."), listing(&reported));

        std::env::set_current_dir(&restore).unwrap();
        assert_eq!(std::env::current_dir().unwrap(), restore);
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
