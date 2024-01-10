//! Definitions for hooking into the monitor.

pub use crate::runtime::RuntimeThreadControl;
use dynlink::tls::TlsRegion;
use twizzler_runtime_api::{AddrRange, Library, LibraryId};

pub trait MonitorActions {
    fn lookup_library_by_id(&self, id: LibraryId) -> Option<Library>;
    fn lookup_library_name(&self, id: LibraryId, buf: &mut [u8]) -> Option<usize>;
    fn local_primary(&self) -> Option<LibraryId>;
    fn get_segment(&self, id: LibraryId, seg: usize) -> Option<AddrRange>;

    fn allocate_tls_region(&self) -> Option<TlsRegion>;
    fn free_tls_region(&self, tls: TlsRegion);
}

extern "rust-call" {
    fn __do_get_monitor_actions(_a: ()) -> &'static mut dyn MonitorActions;
}

pub fn get_monitor_actions() -> &'static mut dyn MonitorActions {
    unsafe { __do_get_monitor_actions(()) }
}
