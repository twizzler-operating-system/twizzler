#![feature(linkage)]

use twizzler_runtime_api::Runtime;

#[inline]
#[no_mangle]
// Returns a reference to the currently-linked Runtime implementation.
pub fn __twz_get_runtime() -> &'static (dyn Runtime + Sync) {
    todo!()
    //&OUR_RUNTIME
}

//static OUR_RUNTIME: MinimalRuntime = MinimalRuntime {};

// Ensure the compiler doesn't optimize us away.
#[used]
static USE_MARKER: fn() -> &'static (dyn Runtime + Sync) = __twz_get_runtime;
