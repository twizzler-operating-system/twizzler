//! Top level runtime module, managing the basic presentation of the runtime.

use std::sync::atomic::{AtomicU32, Ordering};

mod alloc;
mod core;
mod debug;
mod file;
mod object;
mod process;
mod slot;
mod stdio;
mod thread;
mod time;

pub use thread::RuntimeThreadControl;

/// The runtime trait implementer itself.
pub struct ReferenceRuntime {
    pub(crate) state: AtomicU32,
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
    }
}

impl ReferenceRuntime {
    fn state(&self) -> RuntimeState {
        RuntimeState::from_bits_truncate(self.state.load(Ordering::SeqCst))
    }

    fn set_runtime_ready(&self) {
        self.state
            .fetch_or(RuntimeState::READY.bits(), Ordering::SeqCst);
    }
}

static OUR_RUNTIME: ReferenceRuntime = ReferenceRuntime {
    state: AtomicU32::new(0),
};

#[cfg(feature = "runtime")]
pub(crate) mod do_impl {
    use twizzler_runtime_api::Runtime;

    use super::ReferenceRuntime;

    impl Runtime for ReferenceRuntime {}

    #[inline]
    #[no_mangle]
    // Returns a reference to the currently-linked Runtime implementation.
    pub fn __twz_get_runtime() -> &'static (dyn Runtime + Sync) {
        &super::OUR_RUNTIME
    }

    // Ensure the compiler doesn't optimize us away.
    #[used]
    static USE_MARKER: fn() -> &'static (dyn Runtime + Sync) = __twz_get_runtime;
}
