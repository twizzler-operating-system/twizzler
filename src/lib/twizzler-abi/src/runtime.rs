use twizzler_runtime_api::Runtime;

mod alloc;
mod core;
mod debug;
mod fs;
pub(crate) mod object;
mod process;
mod stdio;
mod thread;
mod time;

#[derive(Default)]
pub struct MinimalRuntime {}

impl Runtime for MinimalRuntime {}

#[inline]
#[no_mangle]
#[linkage = "extern_weak"]
pub fn __twz_get_runtime() -> &'static (dyn Runtime + Sync) {
    &OUR_RUNTIME
}

pub static OUR_RUNTIME: MinimalRuntime = MinimalRuntime {};
