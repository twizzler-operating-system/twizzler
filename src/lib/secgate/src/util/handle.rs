use std::{collections::BTreeMap, num::NonZeroUsize};

use stable_vec::StableVec;
use twizzler_rt_abi::object::ObjID;

/// A handle that can be opened and released.
pub trait Handle {
    /// The error type returned by open.
    type OpenError;

    /// The arguments to open.
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
    handles: BTreeMap<ObjID, StableVec<ServerData>>,
    max: Option<NonZeroUsize>,
}

impl<ServerData> HandleMgr<ServerData> {
    /// Construct a new HandleMgr.
    pub const fn new(max: Option<usize>) -> Self {
        Self {
            handles: BTreeMap::new(),
            max: match max {
                Some(m) => NonZeroUsize::new(m),
                None => None,
            },
        }
    }

    /// Get the maximum number of open handles.
    pub fn max(&self) -> Option<usize> {
        self.max.map(|x| x.get())
    }

    /// Get the total number of open handles across all compartments.
    pub fn total_count(&self) -> usize {
        self.handles
            .values()
            .fold(0, |acc, val| acc + val.num_elements())
    }

    /// Get the number of currently open handles for a given compartment.
    pub fn open_count(&self, comp: ObjID) -> usize {
        self.handles
            .get(&comp)
            .map(|sv| sv.num_elements())
            .unwrap_or(0)
    }

    /// Lookup the server data associated with a descriptor.
    pub fn lookup(&self, comp: ObjID, ds: Descriptor) -> Option<&ServerData> {
        let idx: usize = ds.try_into().ok()?;
        self.handles.get(&comp).and_then(|sv| sv.get(idx))
    }

    /// Lookup the server data associated with a descriptor.
    pub fn lookup_mut(&mut self, comp: ObjID, ds: Descriptor) -> Option<&mut ServerData> {
        let idx: usize = ds.try_into().ok()?;
        self.handles.get_mut(&comp).and_then(|sv| sv.get_mut(idx))
    }

    /// Insert new server data, and return a descriptor for it.
    pub fn insert(&mut self, comp: ObjID, sd: ServerData) -> Option<Descriptor> {
        let entry = self.handles.entry(comp).or_insert_with(|| StableVec::new());
        let idx = entry.next_push_index();
        if let Some(max) = self.max {
            if idx >= max.get() {
                return None;
            }
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



#[cfg(kani)]
mod handle {
    use std::cell::RefCell;

    use super::*;

    struct FooHandle {
        desc: Descriptor,
        x: u32,
        mgr: RefCell<HandleMgr<u32>>,
        removed_data: Option<u32>,
    }

    impl Handle for FooHandle {
        type OpenError = ();

        type OpenInfo = (u32, RefCell<HandleMgr<u32>>);

        fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
        where
            Self: Sized,
        {
            let desc = info.1.borrow_mut().insert(0.into(), info.0).unwrap();
            Ok(Self {
                desc,
                x: info.0,
                mgr: info.1,
                removed_data: None,
            })
        }

        fn release(&mut self) {
            self.removed_data = self.mgr.borrow_mut().remove(0.into(), self.desc);
        }
    }

    //TODO: Very long,  what is this actually proving rn. 
    #[kani::proof]
    #[kani::unwind(2)]
    fn handle() {
        const LIMIT: usize = 100;

        let val = kani::any();
        let info = kani::any();
        kani::assume(val as usize <= LIMIT );
        kani::assume(info as usize <= LIMIT );

        let mgr = RefCell::new(HandleMgr::new(Some(val)));
        let mut foo = FooHandle::open((info, mgr)).unwrap();

        assert_eq!(foo.x, info);
        let sd = foo.mgr.borrow().lookup(0.into(), foo.desc).cloned();
        assert_eq!(sd, Some(info));

        foo.release();
        assert_eq!(foo.removed_data, Some(info));
        assert!(foo.mgr.borrow().lookup(0.into(), foo.desc).is_none());
    }
}

#[cfg(test)]
mod test {
    use std::cell::RefCell;

    use super::*;

    struct FooHandle {
        desc: Descriptor,
        x: u32,
        mgr: RefCell<HandleMgr<u32>>,
        removed_data: Option<u32>,
    }

    impl Handle for FooHandle {
        type OpenError = ();

        type OpenInfo = (u32, RefCell<HandleMgr<u32>>);

        fn open(info: Self::OpenInfo) -> Result<Self, Self::OpenError>
        where
            Self: Sized,
        {
            let desc = info.1.borrow_mut().insert(0.into(), info.0).unwrap();
            Ok(Self {
                desc,
                x: info.0,
                mgr: info.1,
                removed_data: None,
            })
        }

        fn release(&mut self) {
            self.removed_data = self.mgr.borrow_mut().remove(0.into(), self.desc);
        }
    }

    #[test]
    fn handle() {
        let mgr = RefCell::new(HandleMgr::new(Some(8)));
        let mut foo = FooHandle::open((42, mgr)).unwrap();

        assert_eq!(foo.x, 42);
        let sd = foo.mgr.borrow().lookup(0.into(), foo.desc).cloned();
        assert_eq!(sd, Some(42));

        foo.release();
        assert_eq!(foo.removed_data, Some(42));
        assert!(foo.mgr.borrow().lookup(0.into(), foo.desc).is_none());
    }
}
