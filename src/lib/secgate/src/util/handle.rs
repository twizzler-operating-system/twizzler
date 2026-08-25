use core::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, num::NonZeroUsize};

use stable_vec::StableVec;
use twizzler_abi::syscall::sys_object_stat;
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
    /// Counts calls to [`HandleMgr::gc_handles`], so its expensive half can run on a cadence.
    gc_tick: u32,
}

/// How often the expensive half of [`HandleMgr::gc_handles`] runs. `1` is the original behaviour.
///
/// The cheap half -- dropping tables that have become empty -- is a `retain` over a `BTreeMap` and
/// runs every time. The expensive half asks the kernel whether each tracked compartment still
/// exists, which is **one `sys_object_stat` per non-empty table, on every `insert` and every
/// `remove`**.
///
/// Measured on the spawn path: a `CompartmentHandle::lookup` that finds nothing -- same gate, same
/// lock, same map lookup, but no handle created and none dropped -- is **1 us**. One that creates a
/// handle is **347 us**, and dropping that handle is another **312 us**. The gate mechanism is
/// therefore not the cost; the GC is, and it was ~19% of a 3.4 ms compartment spawn.
///
/// Running it on a cadence only delays reclaiming the *table* of a compartment that has already
/// died. Nothing depends on that for correctness: a dead compartment's descriptors are already
/// unreachable, and the empty-table half -- which is what reclaims a live compartment's table once
/// it closes its handles -- still runs every time.
const GC_FULL_EVERY: u32 = 1;

/// Switch for the `HDLGC` counter: cumulative calls, microseconds and `sys_object_stat` syscalls.
const GC_STATS: bool = false;
static GC_CALLS: AtomicU64 = AtomicU64::new(0);
static GC_NS: AtomicU64 = AtomicU64::new(0);
static GC_STATCALLS: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds inside `sys_object_stat` alone, and how many of those returned "gone". A stat on a
/// dead object may be a slower path than one on a live object, which a total cannot show.
static GC_SYSNS: AtomicU64 = AtomicU64::new(0);
static GC_MISSES: AtomicU64 = AtomicU64::new(0);

impl<ServerData> HandleMgr<ServerData> {
    /// Construct a new HandleMgr.
    pub const fn new(max: Option<usize>) -> Self {
        Self {
            handles: BTreeMap::new(),
            gc_tick: 0,
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
        self.gc_handles();

        Some(ds)
    }

    /// Remove a descriptor, returning the server data if present.
    pub fn remove(&mut self, comp: ObjID, ds: Descriptor) -> Option<ServerData> {
        let idx: usize = ds.try_into().ok()?;
        let ret = self.handles.get_mut(&comp)?.remove(idx);
        self.gc_handles();
        ret
    }

    pub fn handles(&self) -> impl Iterator<Item = (ObjID, u32, &ServerData)> {
        self.handles
            .iter()
            .map(|c| c.1.iter().map(|x| (*c.0, x.0 as u32, x.1)))
            .flatten()
    }

    /// Drop handle tables belonging to compartments that are gone.
    ///
    /// Deliberately does *not* call `twz_rt_gc`. This runs from `insert`/`remove`, i.e. under
    /// whatever lock the server holds over its `HandleMgr` -- and in the monitor that is a monitor
    /// lock, while `twz_rt_gc` reaches `THREAD_MGR` in the monitor's own runtime. A spawn holds
    /// `THREAD_MGR` and calls a monitor gate, so the pair deadlocked: `lostwake/round765` caught
    /// `get_compartment_handle` holding `comp_lookup` here and waiting on `THREAD_MGR`.
    pub fn gc_handles(&mut self) {
        let _t0 = GC_STATS.then(crate::now_ns);
        // Free, and the common case: a table with nothing in it is dead whoever owns it.
        self.handles.retain(|_, sv| !sv.is_empty());
        self.gc_tick = self.gc_tick.wrapping_add(1);
        if GC_FULL_EVERY > 1 && self.gc_tick % GC_FULL_EVERY != 0 {
            return;
        }
        fn sctx_still_valid(id: &ObjID) -> bool {
            if id.raw() == 0 {
                return true;
            }
            let t = GC_STATS.then(crate::now_ns);
            let r = sys_object_stat(*id).is_ok();
            if let Some(t) = t {
                GC_SYSNS.fetch_add(crate::now_ns().saturating_sub(t), Ordering::Relaxed);
                if !r {
                    GC_MISSES.fetch_add(1, Ordering::Relaxed);
                }
            }
            r
        }
        let _n = self.handles.len() as u64;
        self.handles.retain(|id, _| sctx_still_valid(id));
        if let Some(t0) = _t0 {
            // How much of a spawn is really here, measured rather than inferred from a difference
            // between two call shapes. `stats` is the number of `sys_object_stat` syscalls this
            // pass made -- the thing the cadence knob divides.
            let ns = crate::now_ns().saturating_sub(t0);
            let calls = GC_CALLS.fetch_add(1, Ordering::Relaxed) + 1;
            GC_NS.fetch_add(ns, Ordering::Relaxed);
            let stats = GC_STATCALLS.fetch_add(_n, Ordering::Relaxed) + _n;
            if calls % 256 == 0 {
                crate::statlog::record_on(
                    GC_STATS,
                    "HDLGC",
                    calls,
                    &[
                        GC_NS.load(Ordering::Relaxed) / 1000,
                        stats,
                        GC_SYSNS.load(Ordering::Relaxed) / 1000,
                        GC_MISSES.load(Ordering::Relaxed),
                    ],
                );
            }
        }
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
