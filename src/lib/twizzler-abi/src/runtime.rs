use twizzler_runtime_api::Runtime;

mod alloc;
mod core;
mod debug;
mod fs;
pub(crate) mod object;
pub(crate) mod phdrs;
mod process;
mod stdio;
mod thread;
mod time;
pub(crate) mod tls;

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
