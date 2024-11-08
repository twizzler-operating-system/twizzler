//! This mod implements the [twizzler_runtime_api::Runtime] trait as the "minruntime", or minimal
//! runtime implementation. The word minimal is pretty subjective, but here we're roughly saying
//! "it's the runtime that you can use to interact with the kernel and twizzler-abi directly, with
//! no additional support".
//!
//! Additionally, we provide a mechanism for linking our runtime only if no other runtime is linked,
//! via the "extern_weak" linkage attribute on __twz_get_runtime.

use twizzler_runtime_api::Runtime;

mod alloc;
mod core;
mod debug;
mod fs;
mod idcounter;
pub mod load_elf;
pub(crate) mod object;
pub(crate) mod phdrs;
mod process;
mod simple_mutex;
mod stdio;
mod thread;
mod time;
pub(crate) mod tls;
pub(crate) mod upcall;

pub mod syms;

#[derive(Default)]
pub struct MinimalRuntime {}

impl Runtime for MinimalRuntime {}

#[inline]
#[no_mangle]
#[linkage = "weak"]
// Returns a reference to the currently-linked Runtime implementation.
pub fn __twz_get_runtime() -> &'static (dyn Runtime + Sync) {
    &OUR_RUNTIME
}

static OUR_RUNTIME: MinimalRuntime = MinimalRuntime {};

// Ensure the compiler doesn't optimize us away.
#[used]
static USE_MARKER: fn() -> &'static (dyn Runtime + Sync) = __twz_get_runtime;

pub use object::slot::get_kernel_init_info;
