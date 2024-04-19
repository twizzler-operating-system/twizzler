use secgate::Crossing;
use twizzler_runtime_api::{
    AddrRange, DlPhdrInfo, LibraryId, MapError, ObjID, SpawnError, ThreadSpawnArgs,
};

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

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_library_info(
    info: &secgate::GateCallInfo,
    library_id: LibraryId,
) -> Option<LibraryInfo> {
    crate::state::__monitor_rt_get_library_info(info, library_id)
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
    flags: twizzler_runtime_api::MapFlags,
) -> Result<usize, MapError> {
    crate::object::map_object(info, id, flags)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_unmap(info: &secgate::GateCallInfo, slot: usize) {
    crate::object::unmap_object(info, slot)
}
