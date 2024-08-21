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
    compartment: ObjID,
    lib_n: usize,
) -> Option<LibraryInfo> {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let compartment = if compartment.as_u128() == 0 {
        caller
    } else {
        compartment
    };
    monitor.get_library_info(caller, compartment, lib_n)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LibraryInfo {
    pub id: LibraryId,
    pub name_len: usize,
    pub compartment_id: ObjID,
    pub objid: ObjID,
    pub slot: usize,
    pub range: AddrRange,
    pub dl_info: DlPhdrInfo,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CompartmentInfo {
    pub name_len: usize,
    pub id: ObjID,
    pub sctx: ObjID,
    pub flags: u32,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_compartment_info(
    info: &secgate::GateCallInfo,
    compartment: ObjID,
) -> Option<CompartmentInfo> {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let compartment = if compartment.as_u128() == 0 {
        caller
    } else {
        compartment
    };
    monitor.get_compartment_info(caller, compartment)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_compartment_deps(
    info: &secgate::GateCallInfo,
    compartment: ObjID,
    dep_n: usize,
) -> Option<CompartmentInfo> {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let compartment = if compartment.as_u128() == 0 {
        caller
    } else {
        compartment
    };
    monitor.get_compartment_deps(caller, compartment, dep_n)
}

// Safety: the broken part is just DlPhdrInfo. We ensure that any pointers in there are
// intra-compartment.
unsafe impl Crossing for LibraryInfo {}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_load_compartment(
    info: &secgate::GateCallInfo,
    root_id: ObjID,
) -> Result<CompartmentInfo, LoadCompartmentError> {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_compartment(caller, root_id)
}

#[derive(Clone, Copy, Debug)]
pub enum LoadCompartmentError {
    Unknown,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_drop_compartment_handle(info: &secgate::GateCallInfo, id: ObjID) {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_compartment_handle(caller, id)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_load_library(
    info: &secgate::GateCallInfo,
    id: ObjID,
) -> Result<LibraryInfo, LoadLibraryError> {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_library(caller, id)
}

#[derive(Clone, Copy, Debug)]
pub enum LoadLibraryError {
    Unknown,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_drop_library_handle(info: &secgate::GateCallInfo, id: LibraryId) {
    use crate::api::MONITOR_INSTANCE_ID;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_library_handle(caller, id)
}

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
