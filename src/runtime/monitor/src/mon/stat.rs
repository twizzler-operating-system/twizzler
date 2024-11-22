use std::collections::HashSet;

use happylock::ThreadKey;

use super::Monitor;
use crate::gates::{DynlinkStats, HandleStats, MonitorStats};

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
