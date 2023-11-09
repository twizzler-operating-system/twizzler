use tracing::debug;
use twz_rt::monitor::MonitorActions;

#[no_mangle]
pub fn __do_get_monitor_actions() -> &'static dyn MonitorActions {
    &ACTIONS
}

struct MonitorActionsImpl;

impl MonitorActions for MonitorActionsImpl {
    fn lookup_library_by_id(
        &self,
        id: twizzler_runtime_api::LibraryId,
    ) -> Option<twizzler_runtime_api::Library> {
        debug!("got to monitor");
        None
    }
}

static ACTIONS: MonitorActionsImpl = MonitorActionsImpl;
