//! Top level runtime module, managing the basic presentation of the runtime.

use std::sync::atomic::{AtomicU32, Ordering};

mod alloc;
mod core;
mod debug;
mod file;
mod object;
mod process;
mod slot;
mod thread;
mod time;
pub(crate) mod upcall;

use twizzler_abi::simple_mutex::Mutex;
pub use upcall::set_upcall_handler;

use self::object::ObjectHandleManager;

/// The runtime trait implementer itself.
pub struct ReferenceRuntime {
    pub(crate) state: AtomicU32,
    pub(crate) object_manager: Mutex<ObjectHandleManager>,
}

impl std::fmt::Debug for ReferenceRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "RefRun({})",
            if self.state().contains(RuntimeState::READY) {
                "ready"
            } else {
                "not-ready"
            }
        )
    }
}

bitflags::bitflags! {
    /// Various state flags for the runtime.
    pub struct RuntimeState : u32 {
        const READY = 1;
        const IS_MONITOR = 2;
    }
}

impl ReferenceRuntime {
    /// Returns the runtime state flags.
    pub fn state(&self) -> RuntimeState {
        RuntimeState::from_bits_truncate(self.state.load(Ordering::SeqCst))
    }

    /// Set the runtime ready state. If the runtime has not been initialized, the result is
    /// undefined.
    pub unsafe fn set_runtime_ready(&self) {
        self.state
            .fetch_or(RuntimeState::READY.bits(), Ordering::SeqCst);
    }

    fn set_is_monitor(&self) {
        self.state
            .fetch_or(RuntimeState::IS_MONITOR.bits(), Ordering::SeqCst);
    }
}

pub static OUR_RUNTIME: ReferenceRuntime = ReferenceRuntime {
    state: AtomicU32::new(0),
    object_manager: Mutex::new(ObjectHandleManager::new()),
};

// These are exported by libunwind, but not re-exported by the standard library that pulls that in.
// Or, at least, that's what it seems like. In any case, they're no-ops in libunwind and musl, so
// this is fine for now.
#[no_mangle]
pub extern "C" fn __register_frame_info() {}
#[no_mangle]
pub extern "C" fn __deregister_frame_info() {}
#[no_mangle]
pub extern "C" fn __cxa_finalize() {}
