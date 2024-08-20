use std::sync::OnceLock;

use happylock::RwLock;

use self::space::Unmapper;

pub(crate) mod compartment;
pub(crate) mod space;
pub(crate) mod thread;

/// A security monitor instance. All monitor logic is implemented as methods for this type.
/// We split the state into the following components: 'space', managing the virtual memory space and
/// mapping objects, 'thread_mgr', which manages all threads owned by the monitor (typically, all
/// threads started by compartments), 'compartments', which manages compartment state, and
/// 'dynlink', which contains the dynamic linker state. The unmapper allows for background unmapping
/// and cleanup of objects and handles.
pub struct Monitor {
    space: RwLock<space::Space>,
    thread_mgr: RwLock<thread::ThreadMgr>,
    compartments: RwLock<compartment::CompartmentMgr>,
    dynlink: RwLock<dynlink::context::Context>,
    unmapper: Unmapper,
}

static MONITOR: OnceLock<Monitor> = OnceLock::new();

/// Get the monitor instance. Panics if called before first call to [set_monitor].
pub fn get_monitor() -> &'static Monitor {
    MONITOR.get().unwrap()
}

/// Set the monitor instance. Can only be called once. Must be called before any call to
/// [get_monitor].
pub fn set_monitor(monitor: Monitor) {
    if MONITOR.set(monitor).is_err() {
        panic!("second call to set_monitor");
    }
}
