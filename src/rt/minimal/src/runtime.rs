//! This mod implements the Twizzler runtime as the "minruntime", or minimal
//! runtime implementation. The word minimal is pretty subjective, but here we're roughly saying
//! "it's the runtime that you can use to interact with the kernel and twizzler-abi directly, with
//! no additional support".
//!
//! Additionally, we provide a mechanism for linking our runtime only if no other runtime is linked,
//! via the "extern_weak" linkage attribute on __twz_get_runtime.

mod alloc;
mod core;
mod debug;
mod fs;
mod idcounter;
//pub mod load_elf;
pub(crate) mod object;
pub(crate) mod phdrs;
mod process;
mod thread;
mod time;
pub(crate) mod tls;
pub(crate) mod upcall;

pub mod syms;

#[derive(Default)]
pub struct MinimalRuntime {}

pub(crate) static OUR_RUNTIME: MinimalRuntime = MinimalRuntime {};

pub use object::slot::get_kernel_init_info;
