use twizzler_runtime_api::Runtime;

mod core;
mod debug;
mod file;
mod object;
mod process;
mod stdio;
mod thread;
mod time;

pub struct ReferenceRuntime {}

impl Runtime for ReferenceRuntime {}

#[inline]
#[no_mangle]
// Returns a reference to the currently-linked Runtime implementation.
pub fn __twz_get_runtime() -> &'static (dyn Runtime + Sync) {
    &OUR_RUNTIME
}

static OUR_RUNTIME: ReferenceRuntime = ReferenceRuntime {};

// Ensure the compiler doesn't optimize us away.
#[used]
static USE_MARKER: fn() -> &'static (dyn Runtime + Sync) = __twz_get_runtime;
