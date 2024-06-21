#[cfg(feature = "secgate-impl")]
use monitor_api::MappedObjectAddrs;
use secgate::Crossing;
use twizzler_runtime_api::{
    AddrRange, DlPhdrInfo, LibraryId, MapError, MapFlags, ObjID, SpawnError, ThreadSpawnArgs,
};

#[cfg(not(feature = "secgate-impl"))]
use crate::MappedObjectAddrs;

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
    crate::api::spawn_thread(info.source_context(), args, thread_pointer, stack_pointer)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_comp_config(info: &secgate::GateCallInfo) -> usize {
    crate::api::get_comp_config(info.source_context()) as usize
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_library_info(
    info: &secgate::GateCallInfo,
    library_id: LibraryId,
) -> Option<LibraryInfo> {
    crate::api::get_library_info(info, library_id)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LibraryInfo {
    pub objid: ObjID,
    pub slot: usize,
    pub range: AddrRange,
    pub dl_info: DlPhdrInfo,
    pub next_id: Option<LibraryId>,
}

// Safety: the broken part is just DlPhdrInfo. We ensure that any pointers in there are
// intra-compartment.
unsafe impl Crossing for LibraryInfo {}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_map(
    info: &secgate::GateCallInfo,
    id: ObjID,
    flags: MapFlags,
) -> Result<MappedObjectAddrs, MapError> {
    crate::api::map_object(info.source_context(), id, flags)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_unmap(info: &secgate::GateCallInfo, id: ObjID, flags: MapFlags) {
    crate::api::drop_map(info.source_context(), id, flags)
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
pub enum MonitorCompControlCmd {
    RuntimeReady,
    RuntimePostMain,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_comp_ctrl(
    info: &secgate::GateCallInfo,
    cmd: MonitorCompControlCmd,
) -> Option<i32> {
    crate::api::compartment_ctrl(info, cmd)
}
