use std::sync::OnceLock;

use happylock::RwLock;

use self::space::Unmapper;

mod compartment;
mod space;
mod thread;

pub struct Monitor {
    space: RwLock<space::Space>,
    thread_mgr: RwLock<thread::ThreadMgr>,
    compartments: RwLock<compartment::CompartmentMgr>,
    dynlink: RwLock<dynlink::context::Context>,
    unmapper: Unmapper,
}

static MONITOR: OnceLock<Monitor> = OnceLock::new();

pub fn get_monitor() -> &'static Monitor {
    MONITOR.get().unwrap()
}

pub fn set_monitor(monitor: Monitor) {
    if MONITOR.set(monitor).is_err() {
        panic!("second call to set_monitor");
    }
}
