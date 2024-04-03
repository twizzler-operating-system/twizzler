use twizzler_runtime_api::{MapFlags, ObjID};

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct MapInfo {
    pub(crate) id: ObjID,
    pub(crate) flags: MapFlags,
}
