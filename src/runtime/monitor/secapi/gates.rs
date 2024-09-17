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
    let monitor = crate::mon::get_monitor();
    monitor.spawn_compartment_thread(
        info.source_context().unwrap_or(0.into()),
        args,
        stack_pointer,
        thread_pointer,
    )
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_comp_config(info: &secgate::GateCallInfo) -> usize {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    monitor
        .get_comp_config(info.source_context().unwrap_or(MONITOR_INSTANCE_ID))
        .map(|ptr| ptr as usize)
        .unwrap_or(0)
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
) -> Result<crate::MappedObjectAddrs, MapError> {
    use twz_rt::{RuntimeState, OUR_RUNTIME};

    use crate::{api::MONITOR_INSTANCE_ID, mon::space::MapInfo};
    if OUR_RUNTIME.state().contains(RuntimeState::READY) {
        // Are we recursing from the monitor, with a lock held? In that case, use early_object_map
        // to map the object. This will leak this mapping, but this is both rare, and then
        // since the mapping is leaked, it can be used as an allocator object indefinitely
        // (not currently implemented). Make sure the ThreadKey drops.
        let is_monitor_recursed =
            { happylock::ThreadKey::get().is_none() && info.source_context().is_none() };
        if is_monitor_recursed {
            tracing::debug!(
                "performing early object mapping (recursed), {} {:?}",
                id,
                flags
            );
            return Ok(crate::mon::early_object_map(MapInfo { id, flags }));
        }
        let monitor = crate::mon::get_monitor();
        monitor
            .map_object(
                info.source_context().unwrap_or(MONITOR_INSTANCE_ID),
                MapInfo { id, flags },
            )
            .map(|handle| handle.addrs())
    } else {
        // We need to use early_object_map, since the monitor hasn't finished initializing, but
        // still needs to allocate (which may involve mapping an object).
        tracing::debug!("performing early object mapping, {} {:?}", id, flags);
        Ok(crate::mon::early_object_map(MapInfo { id, flags }))
    }
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_unmap(
    info: &secgate::GateCallInfo,
    _slot: usize,
    id: ObjID,
    flags: twizzler_runtime_api::MapFlags,
) {
    use twz_rt::{RuntimeState, OUR_RUNTIME};
    if OUR_RUNTIME.state().contains(RuntimeState::READY) {
        let monitor = crate::mon::get_monitor();
        let key = happylock::ThreadKey::get().unwrap();
        monitor
            .comp_mgr
            .write(key)
            .get_mut(
                info.source_context()
                    .unwrap_or(crate::api::MONITOR_INSTANCE_ID),
            )
            .unwrap()
            .unmap_object(crate::mon::space::MapInfo { id, flags })
    }
}
