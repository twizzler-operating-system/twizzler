//! Definitions for hooking into the monitor.

use twizzler_runtime_api::{AddrRange, Library, LibraryId};

pub trait MonitorActions {
    fn lookup_library_by_id(&self, id: LibraryId) -> Option<Library>;
    fn lookup_library_name(&self, id: LibraryId, buf: &mut [u8]) -> Result<usize, ()>;
    fn local_primary(&self) -> Option<LibraryId>;
    fn get_segment(&self, id: LibraryId, seg: usize) -> Option<AddrRange>;
}

extern "rust-call" {
    fn __do_get_monitor_actions(_a: ()) -> &'static dyn MonitorActions;
}

pub fn get_monitor_actions() -> &'static dyn MonitorActions {
    unsafe { __do_get_monitor_actions(()) }
}
