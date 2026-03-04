use dynlink::context::NewCompartmentFlags;
use monitor_api::{
    CompartmentInfoRaw, LibraryInfoRaw, MonitorCompControlCmd, MonitorStats, PostSignalFlags,
    ThreadInfo, MONITOR_INSTANCE_ID,
};
use secgate::util::Descriptor;
use twizzler_rt_abi::{
    error::{ArgumentError, ResourceError, TwzError},
    object::ObjID,
    thread::ThreadSpawnArgs,
};

extern "C-unwind" {
    fn __is_monitor_ready() -> bool;
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_spawn_thread(
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<ObjID, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    monitor.spawn_compartment_thread(
        info.source_context().unwrap_or(0.into()),
        args,
        stack_pointer,
        thread_pointer,
    )
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_comp_config() -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    Ok(monitor
        .get_comp_config(info.source_context().unwrap_or(MONITOR_INSTANCE_ID))
        .map(|ptr| ptr as usize)
        .unwrap_or(0))
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_library_info(desc: Descriptor) -> Result<LibraryInfoRaw, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let instance = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let thread = info.thread_id();
    monitor.get_library_info(instance, thread, desc)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_library_handle(
    compartment: Option<Descriptor>,
    lib_n: usize,
) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_library_handle(caller, compartment, lib_n)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_compartment_handle(compartment: ObjID) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    let compartment = if compartment.raw() == 0 {
        caller
    } else {
        compartment
    };
    monitor.get_compartment_handle(caller, compartment)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_compartment_info(
    desc: Option<Descriptor>,
) -> Result<CompartmentInfoRaw, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_info(caller, info.thread_id(), desc)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_compartment_dynamic_gate(
    desc: Option<Descriptor>,
    name_len: usize,
) -> Result<usize, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_gate_address(caller, info.thread_id(), desc, name_len)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_compartment_deps(
    desc: Option<Descriptor>,
    dep_n: usize,
) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_deps(caller, desc, dep_n)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_compartment_thread(
    desc: Option<Descriptor>,
    dep_n: usize,
) -> Result<ThreadInfo, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_compartment_thread_info(caller, desc, dep_n)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_lookup_compartment(name_len: usize) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.lookup_compartment(caller, info.thread_id(), name_len)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_load_compartment(
    root_obj: ObjID,
    name_len: u64,
    args_len: u64,
    env_len: u64,
    flags: u32,
    config: u64,
) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_compartment(
        caller,
        info.thread_id(),
        root_obj,
        name_len as usize,
        args_len as usize,
        env_len as usize,
        NewCompartmentFlags::from_bits(flags).ok_or(ArgumentError::InvalidArgument)?,
        config as usize as *const _,
    )
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_compartment_wait(desc: Option<Descriptor>, flags: u64) -> Result<u64, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    Ok(monitor.compartment_wait(caller, desc, flags))
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_drop_compartment_handle(desc: Descriptor) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_compartment_handle(caller, desc);
    Ok(())
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_load_library(
    compartment: Option<Descriptor>,
    id: ObjID,
) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.load_library(caller, id, compartment)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_drop_library_handle(desc: Descriptor) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.drop_library_handle(caller, desc);
    Ok(())
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_object_map(
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
) -> Result<crate::MappedObjectAddrs, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
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

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_object_pair_map(
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
    id2: ObjID,
    flags2: twizzler_rt_abi::object::MapFlags,
) -> Result<(crate::MappedObjectAddrs, crate::MappedObjectAddrs), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
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

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_object_unmap(
    id: ObjID,
    flags: twizzler_rt_abi::object::MapFlags,
) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    if unsafe { __is_monitor_ready() } {
        let monitor = crate::mon::get_monitor();
        monitor.unmap_object(
            info.source_context().unwrap_or(MONITOR_INSTANCE_ID),
            crate::mon::space::MapInfo { id, flags },
        );
    }
    Ok(())
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_get_thread_simple_buffer() -> Result<ObjID, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.get_thread_simple_buffer(caller, info.thread_id())
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_comp_ctrl(cmd: MonitorCompControlCmd) -> Result<Option<i32>, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    Ok(monitor.compartment_ctrl(&info, cmd))
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_stats() -> Result<MonitorStats, TwzError> {
    let monitor = crate::mon::get_monitor();
    Ok(monitor.stats())
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_post_signal(
    comp: Option<ObjID>,
    signal: u64,
    flags: PostSignalFlags,
) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    monitor.post_signal(&info, comp, signal, flags)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_set_controller(comp: ObjID, controller: ObjID) -> Result<(), TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let comp = if comp.raw() == 0 {
        info.source_context().unwrap_or(MONITOR_INSTANCE_ID)
    } else {
        comp
    };
    monitor.set_controller(&info, comp, controller)
}

#[secgate::entry(lib = "monitor-api")]
pub fn monitor_rt_lookup_compartment_id(id: ObjID) -> Result<Descriptor, TwzError> {
    let info = secgate::get_caller().ok_or(TwzError::NOT_SUPPORTED)?;
    let monitor = crate::mon::get_monitor();
    let caller = info.source_context().unwrap_or(MONITOR_INSTANCE_ID);
    monitor.lookup_compartment_id(caller, info.thread_id(), id)
}
