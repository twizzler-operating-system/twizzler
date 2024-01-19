#![feature(naked_functions)]
use twizzler_abi::object::ObjID;
use twizzler_runtime_api::{SpawnError, ThreadSpawnArgs};

extern "Rust" {
    pub fn __monitor_rt_spawn_thread(
        args: ThreadSpawnArgs,
        thread_pointer: usize,
        stack_pointer: usize,
    ) -> Result<ObjID, SpawnError>;
}

#[secgate::secure_gate]
pub fn monitor_rt_spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    unsafe { __monitor_rt_spawn_thread(args, thread_pointer, stack_pointer) }
}
