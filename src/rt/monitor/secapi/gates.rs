use std::fmt::Debug;

use dynlink::context::NewCompartmentFlags;
use secgate::{util::Descriptor, Crossing};
use twizzler_rt_abi::{
    debug::DlPhdrInfo,
    error::{ArgumentError, ResourceError, TwzError},
    object::ObjID,
    thread::ThreadSpawnArgs,
};

extern "C-unwind" {
    fn __is_monitor_ready() -> bool;
}

/// Reserved instance ID for the security monitor.
pub const MONITOR_INSTANCE_ID: ObjID = ObjID::new(0);

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
) -> Result<ObjID, TwzError> {
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
pub fn monitor_rt_get_comp_config(info: &secgate::GateCallInfo) -> Result<usize, TwzError> {
    let monitor = crate::mon::get_monitor();
    Ok(monitor
        .get_comp_config(info.source_context().unwrap_or(MONITOR_INSTANCE_ID))
        .map(|ptr| ptr as usize)
        .unwrap_or(0))
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_library_info(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
) -> Result<LibraryInfo, TwzError> {
    let monitor = crate::mon::get_monitor();
    let instance = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let thread = info.thread_id();
    monitor.get_library_info(instance, thread, desc)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_library_handle(
    info: &secgate::GateCallInfo,
    compartment: Option<Descriptor>,
    lib_n: usize,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_library_handle(caller, compartment, lib_n)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct LibraryInfo {
    pub name_len: usize,
    pub compartment_id: ObjID,
    pub objid: ObjID,
    pub slot: usize,
    pub start: *const u8,
    pub len: usize,
    pub dl_info: DlPhdrInfo,
    pub desc: Descriptor,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CompartmentInfo {
    pub name_len: usize,
    pub id: ObjID,
    pub sctx: ObjID,
    pub flags: u64,
    pub nr_libs: usize,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_compartment_handle(
    info: &secgate::GateCallInfo,
    compartment: ObjID,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let compartment = if compartment.raw() == 0 {
        caller
    } else {
        compartment
    };
    monitor.get_compartment_handle(caller, compartment)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_compartment_info(
    info: &secgate::GateCallInfo,
    desc: Option<Descriptor>,
) -> Result<CompartmentInfo, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_info(caller, info.thread_id(), desc)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_compartment_dynamic_gate(
    info: &secgate::GateCallInfo,
    desc: Option<Descriptor>,
    name_len: usize,
) -> Result<usize, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_gate_address(caller, info.thread_id(), desc, name_len)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_compartment_deps(
    info: &secgate::GateCallInfo,
    desc: Option<Descriptor>,
    dep_n: usize,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_deps(caller, desc, dep_n)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_lookup_compartment(
    info: &secgate::GateCallInfo,
    name_len: usize,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.lookup_compartment(caller, info.thread_id(), name_len)
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
    name_len: u64,
    args_len: u64,
    env_len: u64,
    flags: u32,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_compartment(
        caller,
        info.thread_id(),
        name_len as usize,
        args_len as usize,
        env_len as usize,
        NewCompartmentFlags::from_bits(flags).ok_or(ArgumentError::InvalidArgument)?,
    )
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_compartment_wait(
    info: &secgate::GateCallInfo,
    desc: Option<Descriptor>,
    flags: u64,
) -> Result<u64, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    Ok(monitor.compartment_wait(caller, desc, flags))
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_drop_compartment_handle(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
) -> Result<(), TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_compartment_handle(caller, desc);
    Ok(())
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_load_library(
    info: &secgate::GateCallInfo,
    compartment: Option<Descriptor>,
    id: ObjID,
) -> Result<Descriptor, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_library(caller, id, compartment)
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_drop_library_handle(
    info: &secgate::GateCallInfo,
    desc: Descriptor,
) -> Result<(), TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_library_handle(caller, desc);
    Ok(())
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_map(
    info: &secgate::GateCallInfo,
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
) -> Result<crate::MappedObjectAddrs, TwzError> {
    use crate::mon::space::MapInfo;
    if unsafe { __is_monitor_ready() } {
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
pub fn monitor_rt_object_pair_map(
    info: &secgate::GateCallInfo,
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
    id2: ObjID,
    flags2: twizzler_rt_abi::object::MapFlags,
) -> Result<(crate::MappedObjectAddrs, crate::MappedObjectAddrs), TwzError> {
    use crate::mon::space::MapInfo;
    if unsafe { !__is_monitor_ready() } {
        return Err(ResourceError::Unavailable.into());
    }
    let monitor = crate::mon::get_monitor();
    monitor
        .map_pair(
            info.source_context().unwrap_or(MONITOR_INSTANCE_ID),
            MapInfo { id, flags },
            MapInfo {
                id: id2,
                flags: flags2,
            },
        )
        .map(|(one, two)| (one.addrs(), two.addrs()))
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_object_unmap(
    info: &secgate::GateCallInfo,
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
) -> Result<(), TwzError> {
    if unsafe { __is_monitor_ready() } {
        let monitor = crate::mon::get_monitor();
        monitor.unmap_object(
            info.source_context().unwrap_or(MONITOR_INSTANCE_ID),
            crate::mon::space::MapInfo { id, flags },
        );
    }
    Ok(())
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_get_thread_simple_buffer(
    info: &secgate::GateCallInfo,
) -> Result<ObjID, TwzError> {
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_thread_simple_buffer(caller, info.thread_id())
}

#[derive(Debug, Copy, Clone)]
#[repr(C)]
#[allow(dead_code)]
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
) -> Result<Option<i32>, TwzError> {
    let monitor = crate::mon::get_monitor();
    Ok(monitor.compartment_ctrl(info, cmd))
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct MonitorStats {
    pub space: SpaceStats,
    pub thread_mgr: ThreadMgrStats,
    pub comp_mgr: CompartmentMgrStats,
    pub handles: HandleStats,
    pub dynlink: DynlinkStats,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct SpaceStats {
    pub mapped: usize,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct ThreadMgrStats {
    pub nr_threads: usize,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct CompartmentMgrStats {
    pub nr_compartments: usize,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct HandleStats {
    pub nr_comp_handles: usize,
    pub nr_lib_handles: usize,
}

#[derive(Copy, Clone, Debug)]
#[repr(C)]
pub struct DynlinkStats {
    pub nr_libs: usize,
    pub nr_comps: usize,
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_stats(_info: &secgate::GateCallInfo) -> Result<MonitorStats, TwzError> {
    let monitor = crate::mon::get_monitor();
    Ok(monitor.stats())
}

#[cfg_attr(feature = "secgate-impl", secgate::secure_gate(options(info)))]
#[cfg_attr(
    not(feature = "secgate-impl"),
    secgate::secure_gate(options(info, api))
)]
pub fn monitor_rt_set_nameroot(info: &secgate::GateCallInfo, root: ObjID) -> Result<(), TwzError> {
    let monitor = crate::mon::get_monitor();
    monitor.set_nameroot(info, root)
}
