use std::collections::HashMap;

use stable_vec::StableVec;
use twizzler_runtime_api::ObjID;

/// A handle that can be opened and released.
pub trait Handle {
    type OpenError;
    type OpenInfo;

    /// Open a handle.
    fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
    where
        Self: Sized;

    /// Release a handle. After this, the handle should not be used.
    fn release(&mut self);
}

/// A handle descriptor.
pub type Descriptor = u32;

/// A manager for open handles, per compartment.
#[derive(Default, Clone)]
pub struct HandleMgr<ServerData> {
    handles: HashMap<ObjID, StableVec<ServerData>>,
    max: usize,
}

impl<ServerData> HandleMgr<ServerData> {
    /// Construct a new HandleMgr.
    pub fn new(max: usize) -> Self {
        Self {
            handles: HashMap::new(),
            max,
        }
    }

    /// Lookup the server data associated with a descriptor.
    pub fn lookup(&self, comp: ObjID, ds: Descriptor) -> Option<&ServerData> {
        let idx: usize = ds.try_into().ok()?;
        self.handles.get(&comp).and_then(|sv| sv.get(idx))
    }

    /// Insert new server data, and return a descriptor for it.
    pub fn insert(&mut self, comp: ObjID, sd: ServerData) -> Option<Descriptor> {
        let entry = self.handles.entry(comp).or_insert_with(|| StableVec::new());
        let idx = entry.next_push_index();
        if idx >= self.max && self.max > 0 {
            return None;
        }
        let ds: Descriptor = idx.try_into().ok()?;
        let pushed_idx = entry.push(sd);
        debug_assert_eq!(pushed_idx, idx);

        Some(ds)
    }

    /// Remove a descriptor, returning the server data if present.
    pub fn remove(&mut self, comp: ObjID, ds: Descriptor) -> Option<ServerData> {
        let idx: usize = ds.try_into().ok()?;
        self.handles.get_mut(&comp)?.remove(idx)
    }
}
