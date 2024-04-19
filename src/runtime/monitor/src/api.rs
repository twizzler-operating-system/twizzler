use monitor_api::SharedCompConfig;
use secgate::GateCallInfo;
use twizzler_runtime_api::{LibraryId, MapError, MapFlags, ObjID, SpawnError, ThreadSpawnArgs};

use crate::{
    compman::COMPMAN,
    gates::LibraryInfo,
    mapman::MappedObjectAddrs,
    threadman::{jump_into_compartment, start_managed_thread, ManagedThread, ManagedThreadRef},
};

pub const MONITOR_INSTANCE_ID: ObjID = ObjID::new(0);

/// Maps an object into a specified compartment, or the monitor compartment if comp is None.
pub fn map_object(
    comp: Option<ObjID>,
    id: ObjID,
    flags: MapFlags,
) -> Result<MappedObjectAddrs, MapError> {
    COMPMAN
        .map_object(comp.unwrap_or(MONITOR_INSTANCE_ID), id, flags)
        .map(|mh| mh.addrs())
}

/// Indicates that the given map has been dropped, and the monitor can consider it freed by the calling compartment.
pub fn drop_map(comp: Option<ObjID>, id: ObjID, flags: MapFlags) {
    let _ = COMPMAN.unmap_object(comp.unwrap_or(MONITOR_INSTANCE_ID), id, flags);
}

/// Get information about a library, from a given compartments perspective.
pub fn get_library_info(info: &GateCallInfo, id: LibraryId) -> Option<LibraryInfo> {
    todo!()
}

/// Spawn a thread into the given compartment.
pub fn spawn_thread(
    comp_id: ObjID,
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_start: usize,
) -> Result<twizzler_runtime_api::ObjID, SpawnError> {
    let managed_thread = start_managed_thread(move || unsafe {
        jump_into_compartment(
            comp_id,
            stack_start + args.stack_size,
            thread_pointer,
            args.start,
            args.arg,
        )
    })?;

    Ok(managed_thread.id)
}

/// Get the caller's compartment configuration pointer.
pub fn get_comp_config(comp_id: Option<ObjID>) -> *const SharedCompConfig {
    COMPMAN
        .get_comp_inner(comp_id.unwrap_or(MONITOR_INSTANCE_ID))
        .map(|comp| comp.lock().unwrap().compartment_config() as *const _)
        .unwrap_or(core::ptr::null())
}
