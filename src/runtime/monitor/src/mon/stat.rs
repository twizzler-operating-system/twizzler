use std::collections::HashSet;

use happylock::ThreadKey;

use super::Monitor;

#[derive(Copy, Clone, Debug)]
pub struct MonitorStats {
    pub space: SpaceStats,
    pub thread_mgr: ThreadMgrStats,
    pub comp_mgr: CompartmentMgrStats,
    pub handles: HandleStats,
    pub dynlink: DynlinkStats,
}

#[derive(Copy, Clone, Debug)]
pub struct SpaceStats {
    pub mapped: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct ThreadMgrStats {
    pub nr_threads: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct CompartmentMgrStats {
    pub nr_compartments: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct HandleStats {
    pub nr_comp_handles: usize,
    pub nr_lib_handles: usize,
}

#[derive(Copy, Clone, Debug)]
pub struct DynlinkStats {
    pub nr_libs: usize,
    pub nr_comps: usize,
}

impl Monitor {
    pub fn stats(&self) -> MonitorStats {
        let (
            ref mut space,
            ref mut threads,
            ref mut comp,
            ref mut dynlink,
            ref mut lib_handles,
            ref mut comp_handles,
        ) = *self.locks.lock(ThreadKey::get().unwrap());
        let mut nr_libs = 0;
        let mut compset = HashSet::new();
        for lib in dynlink.libraries() {
            compset.insert(lib.compartment());
            nr_libs += 1;
        }
        let dynlink = DynlinkStats {
            nr_libs,
            nr_comps: compset.len(),
        };
        let handles = HandleStats {
            nr_comp_handles: comp_handles.total_count(),
            nr_lib_handles: lib_handles.total_count(),
        };
        MonitorStats {
            space: space.stat(),
            thread_mgr: threads.stat(),
            comp_mgr: comp.stat(),
            handles,
            dynlink,
        }
    }
}
