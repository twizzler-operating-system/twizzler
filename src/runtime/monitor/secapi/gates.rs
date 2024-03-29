use twizzler_abi::object::ObjID;
use twizzler_runtime_api::{SpawnError, ThreadSpawnArgs};

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_spawn_thread(
    info: &secgate::GateCallInfo,
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, SpawnError> {
    crate::thread::__monitor_rt_spawn_thread(
        info.source_context().unwrap_or(0.into()),
        args,
        thread_pointer,
        stack_pointer,
    )
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_comp_config(info: &secgate::GateCallInfo) -> usize {
    crate::thread::__monitor_rt_get_comp_config(info.source_context().unwrap_or(0.into())) as usize
}
