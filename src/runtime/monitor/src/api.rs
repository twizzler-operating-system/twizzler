use monitor_api::{MappedObjectAddrs, SharedCompConfig};
use secgate::GateCallInfo;
use twizzler_abi::syscall::SctxAttachError;
use twizzler_runtime_api::{LibraryId, MapError, MapFlags, ObjID, SpawnError, ThreadSpawnArgs};
use twz_rt::preinit_println;

use crate::{
    compman::{
        runcomp::{COMP_IS_BINARY, COMP_THREAD_CAN_EXIT},
        COMPMAN,
    },
    gates::{LibraryInfo, MonitorCompControlCmd},
    threadman::{jump_into_compartment, start_managed_thread},
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

/// Indicates that the given map has been dropped, and the monitor can consider it freed by the
/// calling compartment.
pub fn drop_map(comp: Option<ObjID>, id: ObjID, flags: MapFlags) {
    let _ = COMPMAN.unmap_object(comp.unwrap_or(MONITOR_INSTANCE_ID), id, flags);
}

/// Get information about a library, from a given compartments perspective.
pub fn get_library_info(info: &GateCallInfo, id: LibraryId) -> Option<LibraryInfo> {
    None
}

/// Spawn a thread into the given compartment.
pub fn spawn_thread(
    comp_id: Option<ObjID>,
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_start: usize,
) -> Result<twizzler_runtime_api::ObjID, SpawnError> {
    let managed_thread = start_managed_thread(move || unsafe {
        if let Some(comp_id) = comp_id {
            tracing::debug!("attaching to {:?}", comp_id);
            if let Err(e) = twizzler_abi::syscall::sys_sctx_attach(comp_id) {
                if !matches!(e, SctxAttachError::AlreadyAttached) {
                    tracing::warn!("thread failed to attach to compartment: {}", e);
                    return;
                }
            }
        }
        jump_into_compartment(
            comp_id.unwrap_or(MONITOR_INSTANCE_ID),
            stack_start,
            args.stack_size,
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

pub fn compartment_ctrl(info: &GateCallInfo, cmd: MonitorCompControlCmd) -> Option<i32> {
    tracing::debug!("comp ctrl: {:?} {:?}", info, cmd);
    match cmd {
        MonitorCompControlCmd::RuntimeReady => COMPMAN
            .get_comp_inner(info.source_context().unwrap_or(MONITOR_INSTANCE_ID))
            .map(|comp| {
                comp.lock().unwrap().set_ready();
                if comp.lock().unwrap().has_flag(COMP_IS_BINARY) {
                    None
                } else {
                    Some(0)
                }
            })
            .flatten(),

        MonitorCompControlCmd::RuntimePostMain => {
            let waiter = COMPMAN.with_compartment(
                info.source_context().unwrap_or(MONITOR_INSTANCE_ID),
                |rc| {
                    rc.with_inner(|inner| {
                        if inner.has_flag(COMP_IS_BINARY) {
                            inner.set_flag(COMP_THREAD_CAN_EXIT)
                        }
                    });
                    rc.ready_waiter(COMP_THREAD_CAN_EXIT)
                },
            );
            if let Some(waiter) = waiter {
                waiter.wait();
            }
            None
        }
    }
}
