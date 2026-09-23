//! What each cpu reports about its place in the machine, for `mp::boot_all_secondaries` to
//! assemble into the [`super::sched::CPUTopoNode`] tree.

use alloc::vec::Vec;

pub use twizzler_abi::syscall::CacheKind;

use super::sched::CPUTopoType;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheDesc {
    /// 1 is closest to the core.
    pub level: u8,
    pub kind: CacheKind,
    /// Capacity in bytes.
    pub size: u64,
    pub line_size: u32,
    pub ways: u32,
    pub sets: u32,
    pub inclusive: bool,
    pub fully_assoc: bool,
}

/// One node on the path: which child of the level above, and what that node groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopoStep {
    pub index: usize,
    pub kind: CPUTopoType,
}

/// A cpu's path through the tree, root-most first, plus every cache it can reach.
///
/// Every cpu must produce the same number of steps, or the tree has leaves at different depths
/// and `find_cpu` stops meaning "this cpu's core".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TopoPath {
    pub steps: Vec<TopoStep>,
    /// Each cache with the depth of the node it is shared at: 0 is the root, `d` is the node
    /// `steps[d - 1]` names.
    pub caches: Vec<(usize, CacheDesc)>,
}
