//! Definitions for hooking into the monitor.

use std::sync::OnceLock;

pub use crate::runtime::RuntimeThreadControl;
use monitor_api::SharedCompConfig;
use twizzler_runtime_api::{AddrRange, Library, LibraryId};

pub trait MonitorActions {
    fn lookup_library_by_id(&self, id: LibraryId) -> Option<Library>;
    fn lookup_library_name(&self, id: LibraryId, buf: &mut [u8]) -> Option<usize>;
    fn local_primary(&self) -> Option<LibraryId>;
    fn get_segment(&self, id: LibraryId, seg: usize) -> Option<AddrRange>;
}

extern "rust-call" {
    fn __do_get_monitor_actions(_a: ()) -> &'static mut dyn MonitorActions;
}

pub fn get_monitor_actions() -> &'static mut dyn MonitorActions {
    unsafe { __do_get_monitor_actions(()) }
}

pub fn get_comp_config() -> &'static SharedCompConfig {
    static COMP_CONFIG: OnceLock<&'static SharedCompConfig> = OnceLock::new();
    COMP_CONFIG.get_or_init(|| unsafe {
        (match monitor_api::monitor_rt_get_comp_config() {
            secgate::SecGateReturn::Success(val) => val,
            _ => {
                panic!("failed to get compartment config from monitor")
            }
        } as *const SharedCompConfig)
            .as_ref()
            .unwrap()
    })
}
