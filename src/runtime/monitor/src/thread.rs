use twizzler_runtime_api::ThreadSpawnArgs;

/*
pub fn spawn_thread(args: ThreadSpawnArgs, thread_pointer: usize) {
    let upcall_target = UpcallTarget::new(
        0,
        ,
        0,
        0,
        0.into(),
        [UpcallOptions {
            flags: UpcallFlags::empty(),
            mode: UpcallMode::CallSelf,
        }; UpcallInfo::NR_UPCALLS],
    );

    let thid = unsafe {
        sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
            entry: trampoline as usize,
            stack_base: stack_raw,
            stack_size,
            tls: tls.get_thread_pointer_value(),
            arg: arg_raw,
            flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
            vm_context_handle: None,
            upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
        })
    }
    .map_err(|_| twizzler_runtime_api::SpawnError::KernelError)?;
}
*/
