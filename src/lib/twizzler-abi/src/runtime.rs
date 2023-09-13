use twizzler_runtime_api::Runtime;

mod alloc;
mod core;
mod debug;
mod fs;
mod object;
mod process;
mod stdio;
mod thread;
mod time;

#[derive(Default)]
pub struct MinimalRuntime {}

impl Runtime for MinimalRuntime {
    fn get_runtime<'a>() -> &'a Self {
        todo!()
    }
}
