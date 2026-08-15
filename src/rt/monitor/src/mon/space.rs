use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use miette::IntoDiagnostic;
use monitor_api::{MappedObjectAddrs, SpaceStats};
use twizzler_abi::{
    object::Protections,
    syscall::{
        sys_object_create, sys_object_ctrl, sys_object_map, sys_object_unmap, BackingType,
        CreateTieFlags, CreateTieSpec, DeleteFlags, LifetimeType, ObjectControlCmd, ObjectCreate,
        ObjectCreateFlags, UnmapFlags,
    },
};
use twizzler_rt_abi::{
    bindings::{object_source, object_tie},
    error::{ResourceError, TwzError},
    object::{MapFlags, ObjID},
};

use self::handle::MapHandleInner;

mod handle;
mod unmapper;

pub use handle::MapHandle;
pub use unmapper::Unmapper;

// Temporary instrumentation for the File::open latency hunt (pagerperf.md). `sys_object_map` runs
// with the space lock dropped, so separating it says how much of `Space::map` is real kernel work
// (a cold object needs a pager round trip) versus the two lock acquisitions around it.
mod mapsyscallstats {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static NS: AtomicU64 = AtomicU64::new(0);

    pub fn record(ns: u64) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        let t = NS.fetch_add(ns, Ordering::Relaxed) + ns;
        if secgate::statcadence::report_now(n) {
            secgate::statlog::record("MAPSYSST", n, &[t / 1000]);
        }
    }
}

// Temporary: `Space::map` split into its four phases.
//
// Settled, and not where it was expected. `SPACESTAT` (printed at `RuntimePostMain`) reads
// lock1 ~130 ns, slot ~175 ns, lock2 ~42 ns against a per-miss `sys` of ~110 us -- so the kernel's
// `sys_object_map_in_sctx` is ~99% of this function and everything the monitor does is rounding.
//
// The earlier note here claimed the syscall was "only ~40%" and posed the remainder as a choice
// between sharding the map table and a per-thread slot cache. That remainder is already gone: the
// guard is dropped before the slot allocator is taken (see `map` below), so neither of those is
// worth doing. The cost is inside the kernel.
pub mod spacesplit {
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNT: AtomicU64 = AtomicU64::new(0);
    static HITS: AtomicU64 = AtomicU64::new(0);
    static LOCK1: AtomicU64 = AtomicU64::new(0);
    static SLOT: AtomicU64 = AtomicU64::new(0);
    static SYS: AtomicU64 = AtomicU64::new(0);
    static LOCK2: AtomicU64 = AtomicU64::new(0);

    pub struct Split {
        pub lock1: u64,
        pub slot: u64,
        pub sys: u64,
        pub lock2: u64,
        pub hit: bool,
    }

    /// One line, on demand. The accumulators are unconditional relaxed adds, so this costs nothing
    /// until it prints -- unlike the per-record path below, whose console traffic lands inside the
    /// monitor while it is servicing the very calls being measured.
    pub fn report() {
        let n = COUNT.load(Ordering::Relaxed);
        if n == 0 {
            return;
        }
        let miss = n - HITS.load(Ordering::Relaxed);
        secgate::statcadence::report_forced(format_args!(
            "SPACESTAT {} maps, {} hits, {} misses; per map ns: lock1 {} slot {} lock2 {};              per miss: sys {} ns",
            n,
            HITS.load(Ordering::Relaxed),
            miss,
            LOCK1.load(Ordering::Relaxed) / n,
            SLOT.load(Ordering::Relaxed) / n,
            LOCK2.load(Ordering::Relaxed) / n,
            SYS.load(Ordering::Relaxed) / miss.max(1),
        ));
    }

    pub fn record(s: Split) {
        let n = COUNT.fetch_add(1, Ordering::Relaxed) + 1;
        if s.hit {
            HITS.fetch_add(1, Ordering::Relaxed);
        }
        LOCK1.fetch_add(s.lock1, Ordering::Relaxed);
        SLOT.fetch_add(s.slot, Ordering::Relaxed);
        SYS.fetch_add(s.sys, Ordering::Relaxed);
        LOCK2.fetch_add(s.lock2, Ordering::Relaxed);
        if !secgate::statcadence::report_now(n) {
            return;
        }
        secgate::statlog::record(
            "SPACESPL",
            n,
            &[
                HITS.load(Ordering::Relaxed),
                LOCK1.load(Ordering::Relaxed) / 1000,
                SLOT.load(Ordering::Relaxed) / 1000,
                SYS.load(Ordering::Relaxed) / 1000,
                LOCK2.load(Ordering::Relaxed) / 1000,
            ],
        );
    }
}

#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
/// A mapping of an object and flags.
pub struct MapInfo {
    pub(crate) id: ObjID,
    pub(crate) flags: MapFlags,
}

#[derive(Default)]
/// An address space we can map objects into.
pub struct Space {
    maps: HashMap<MapInfo, MappedObject>,
}

struct MappedObject {
    addrs: MappedObjectAddrs,
    handle_count: usize,
}

fn mapflags_into_prot(flags: MapFlags) -> Protections {
    let mut prot = Protections::empty();
    if flags.contains(MapFlags::READ) {
        prot.insert(Protections::READ);
    }
    if flags.contains(MapFlags::WRITE) {
        prot.insert(Protections::WRITE);
    }
    if flags.contains(MapFlags::EXEC) {
        prot.insert(Protections::EXEC);
    }
    prot
}

extern "C-unwind" {
    fn __monitor_get_slot() -> isize;
    fn __monitor_get_slot_pair(one: *mut usize, two: *mut usize) -> bool;
    fn __monitor_release_pair(one: usize, two: usize);
    fn __monitor_release_slot(slot: usize);
}

impl Space {
    /// Get the stats.
    pub fn stat(&self) -> SpaceStats {
        SpaceStats {
            mapped: self.maps.len(),
            active: self.maps.values().filter(|m| m.handle_count > 0).count(),
        }
    }

    /// Map an object into the space.
    /// `target_sctx` names the security context the kernel should install the mapping in; zero
    /// means the calling thread's active one, which for the monitor is `ObjID(0)` -- the same value
    /// as `KERNEL_SCTX`, so the kernel's eager mapping silently no-ops. See pagerperf.md 17.
    pub fn map<'a>(
        this: &Mutex<Self>,
        info: MapInfo,
        target_sctx: ObjID,
    ) -> Result<MapHandle, TwzError> {
        // Can't use the entry API here because the closure may fail.
        let mut split = spacesplit::Split {
            lock1: 0,
            slot: 0,
            sys: 0,
            lock2: 0,
            hit: true,
        };
        let t_lock1 = std::time::Instant::now();
        let mut guard = crate::lockdiag::watched(this.lock().unwrap());
        split.lock1 = t_lock1.elapsed().as_nanos() as u64;
        let item = match guard.maps.get_mut(&info) {
            Some(item) => item,
            None => {
                split.hit = false;
                // Not yet mapped, so allocate a slot and map it. The slot allocator has its own
                // lock, so taking it here used to nest that lock inside this one and stretch this
                // critical section over it for no reason -- nothing about picking a free slot needs
                // the map table held. Allocating after the drop leaves this lock covering only a
                // hash lookup on the way in and an insert on the way out.
                drop(guard);
                let t_slot = std::time::Instant::now();
                let slot: usize = unsafe { __monitor_get_slot() }
                    .try_into()
                    .ok()
                    .ok_or(ResourceError::OutOfResources)?;
                split.slot = t_slot.elapsed().as_nanos() as u64;

                let t_sys = std::time::Instant::now();
                let res = twizzler_abi::syscall::sys_object_map_in_sctx(
                    None,
                    info.id,
                    slot,
                    mapflags_into_prot(info.flags),
                    info.flags.into(),
                    target_sctx,
                );
                split.sys = t_sys.elapsed().as_nanos() as u64;
                mapsyscallstats::record(split.sys);
                let t_lock2 = std::time::Instant::now();
                guard = crate::lockdiag::watched(this.lock().unwrap());
                split.lock2 = t_lock2.elapsed().as_nanos() as u64;
                let Ok(_) = res else {
                    unsafe {
                        __monitor_release_slot(slot);
                    }
                    return Err(res.unwrap_err());
                };

                // The lock was dropped across the map syscall, so another thread may have mapped
                // this same object meanwhile. Inserting over it would strand that thread's
                // handles on a slot the table no longer names, and lose their references: the
                // first handle to drop would take the count to zero and unmap the surviving
                // slot out from under the others. Keep whichever mapping got there first.
                if guard.maps.contains_key(&info) {
                    let _ = sys_object_unmap(None, slot, UnmapFlags::empty()).inspect_err(|e| {
                        tracing::warn!("failed to unmap redundant mapping of {:?}: {}", info, e)
                    });
                    unsafe {
                        __monitor_release_slot(slot);
                    }
                } else {
                    guard.maps.insert(
                        info,
                        MappedObject {
                            addrs: MappedObjectAddrs::new(slot),
                            handle_count: 0,
                        },
                    );
                }
                // Unwrap-Ok: present either way.
                guard.maps.get_mut(&info).unwrap()
            }
        };

        // New maps will be set to zero, so this is unconditional.
        item.handle_count += 1;
        let addrs = item.addrs;
        drop(guard);
        spacesplit::record(split);
        Ok(Arc::new(MapHandleInner::new(info, addrs)))
    }

    /// Map a pair of objects into the space.
    pub fn map_pair(
        &mut self,
        info: MapInfo,
        info2: MapInfo,
    ) -> Result<(MapHandle, MapHandle), TwzError> {
        // Not yet mapped, so allocate a slot and map it.
        let mut one = 0;
        let mut two = 0;
        if !unsafe { __monitor_get_slot_pair(&mut one, &mut two) } {
            return Err(ResourceError::OutOfResources.into());
        }

        let res = sys_object_map(
            None,
            info.id,
            one,
            mapflags_into_prot(info.flags),
            twizzler_abi::syscall::MapFlags::empty(),
        );
        if res.is_err() {
            unsafe {
                __monitor_release_pair(one, two);
            }
            return Err(res.unwrap_err());
        };

        let res = sys_object_map(
            None,
            info2.id,
            two,
            mapflags_into_prot(info2.flags),
            twizzler_abi::syscall::MapFlags::empty(),
        );
        if res.is_err() {
            let _ = sys_object_unmap(None, one, UnmapFlags::empty())
                .inspect_err(|e| tracing::warn!("failed to unmap first in pair on error: {}", e));
            unsafe {
                __monitor_release_pair(one, two);
            }
            return Err(res.unwrap_err());
        };

        // Deliberately NOT recorded in `maps`. That table is keyed by MapInfo alone and holds one
        // slot per object, but a pair mapping is tied to *this* load: the text must sit adjacent
        // to this load's data object, so the same object gets a different pair per compartment
        // (dynlink's engine uses the source object itself as the text object, so every compartment
        // that loads a library pair-maps the same id). Inserting here would replace the previous
        // load's entry, orphaning its handles from the table and leaving one reference count for
        // two live mappings — after which the first handle to drop unmaps the *other* load's slot
        // while it is still relocating. These handles own their slots outright instead.
        Ok((
            Arc::new(MapHandleInner::new_exclusive(
                info,
                MappedObjectAddrs::new(one),
            )),
            Arc::new(MapHandleInner::new_exclusive(
                info2,
                MappedObjectAddrs::new(two),
            )),
        ))
    }

    /// Remove an object from the space. The actual unmapping syscall only happens once the returned
    /// value from this function is dropped.
    pub fn handle_drop(&mut self, info: MapInfo) -> Option<UnmapOnDrop> {
        // Missing maps in unmap should be ignored.
        let Some(item) = self.maps.get_mut(&info) else {
            tracing::warn!("unmap called for missing object {:?}", info);
            return None;
        };
        if item.handle_count == 0 {
            tracing::error!("unmap called for unmapped object {:?}", info);
            return None;
        }

        // Decrement and maybe actually unmap.
        item.handle_count -= 1;
        if item.handle_count == 0 {
            let slot = item.addrs.slot;
            self.maps.remove(&info);
            Some(UnmapOnDrop { slot })
        } else {
            None
        }
    }

    /// Utility function for creating an object and mapping it, deleting it if the mapping fails.
    pub fn safe_create_and_map_object(
        this: &Mutex<Self>,
        spec: ObjectCreate,
        sources: &[object_source],
        ties: &[object_tie],
        map_flags: MapFlags,
    ) -> miette::Result<MapHandle> {
        let id = sys_object_create(spec, sources, ties).into_diagnostic()?;
        tracing::trace!(
            "created object {} for mapping: {:?} {:?}",
            id,
            spec,
            map_flags
        );

        match Space::map(
            this,
            MapInfo {
                id,
                flags: map_flags,
            },
            ObjID::new(0),
        ) {
            Ok(mh) => Ok(mh),
            Err(me) => {
                if let Err(e) =
                    sys_object_ctrl(id, ObjectControlCmd::Delete(DeleteFlags::empty()), 0, 0)
                {
                    tracing::warn!("failed to delete object {} after map failure {}", e, me);
                }
                Err(me)
            }
        }
        .into_diagnostic()
    }

    pub fn safe_create_and_map_runtime_object(
        this: &Mutex<Self>,
        instance: ObjID,
        map_flags: MapFlags,
    ) -> miette::Result<MapHandle> {
        Space::safe_create_and_map_object(
            this,
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                Some(instance),
                ObjectCreateFlags::DELETE,
                Protections::all(),
            ),
            &[],
            &[CreateTieSpec::new(instance, CreateTieFlags::empty()).into()],
            map_flags,
        )
    }
}

/// Allows us to call handle_drop and do all the hard work in the caller, since
/// the caller probably had to hold a lock to call these functions.
pub(crate) struct UnmapOnDrop {
    slot: usize,
}

impl UnmapOnDrop {
    pub(crate) fn new(slot: usize) -> Self {
        Self { slot }
    }
}

impl Drop for UnmapOnDrop {
    fn drop(&mut self) {
        match sys_object_unmap(None, self.slot, UnmapFlags::empty()) {
            Ok(_) => unsafe {
                __monitor_release_slot(self.slot);
            },
            Err(e) => {
                tracing::warn!("failed to unmap slot {}: {}", self.slot, e);
            }
        }
    }
}

/// Map an object into the address space, without tracking it. This leaks the mapping, but is useful
/// for bootstrapping. See the object mapping gate comments for more details.
pub fn early_object_map(info: MapInfo) -> MappedObjectAddrs {
    let slot = unsafe { __monitor_get_slot() }.try_into().unwrap();

    twizzler_abi::klog_println!(
        "early_object_map: mapping object {} into slot {}",
        info.id,
        slot
    );
    sys_object_map(
        None,
        info.id,
        slot,
        mapflags_into_prot(info.flags),
        twizzler_abi::syscall::MapFlags::empty(),
    )
    .unwrap();

    MappedObjectAddrs::new(slot)
}
