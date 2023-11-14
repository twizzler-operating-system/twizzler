//! Definitions for hooking into the monitor.

use twizzler_runtime_api::{Library, LibraryId};

pub trait MonitorActions {
    fn lookup_library_by_id(&self, id: LibraryId) -> Option<Library>;
}

extern "rust-call" {
    fn __do_get_monitor_actions(_a: ()) -> &'static dyn MonitorActions;
}

pub fn get_monitor_actions() -> &'static dyn MonitorActions {
    unsafe { __do_get_monitor_actions(()) }
}
