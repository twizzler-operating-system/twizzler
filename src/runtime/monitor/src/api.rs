use monitor_api::SharedCompConfig;
use secgate::GateCallInfo;
use twizzler_runtime_api::{
    LibraryId, MapError, MapFlags, ObjID, ObjectHandle, SpawnError, ThreadSpawnArgs,
};

use crate::{compman::COMPMAN, gates::LibraryInfo, mapman::MappedObjectAddrs};

pub const MONITOR_INSTANCE_ID: ObjID = 0;

/// Maps an object into a specified compartment, or the monitor compartment if comp is None.
pub fn map_object(
    comp: Option<ObjID>,
    id: ObjID,
    flags: MapFlags,
) -> Result<MappedObjectAddrs, MapError> {
    COMPMAN.map_object(comp.unwrap_or(MONITOR_INSTANCE_ID), id, flags)
}

/// Indicates that the given map has been dropped, and the monitor can consider it freed by the calling compartment.
pub fn drop_map(comp: Option<ObjID>, id: ObjID, flags: MapFlags) {
    let _ = COMPMAN.unmap_object(comp.unwrap_or(MONITOR_INSTANCE_ID), id, flags);
}

pub fn get_library_info(info: &GateCallInfo, id: LibraryId) -> Option<LibraryInfo> {
    todo!()
}

pub fn spawn_thread(
    src_ctx: ObjID,
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
) -> Result<twizzler_runtime_api::ObjID, SpawnError> {
    todo!()
}

pub fn get_comp_config(comp_id: Option<ObjID>) -> *const SharedCompConfig {
    todo!()
}
