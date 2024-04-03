use secgate::GateCallInfo;
use tracing::warn;
use twizzler_abi::object::Protections;
use twizzler_abi::syscall::{sys_object_map, sys_object_unmap, UnmapFlags};
use twizzler_runtime_api::{MapError, MapFlags, ObjID};

fn mapflags_into_prot(flags: MapFlags) -> Protections {
    let mut prot = Protections::empty();
    if flags.contains(MapFlags::READ) {
        prot.insert(Protections::READ);
    }
    if flags.contains(MapFlags::WRITE) {
        prot.insert(Protections::WRITE);
    }
    if flags.contains(MapFlags::EXEC) {
        prot.insert(Protections::EXEC);
    }
    prot
}

/// Map an object into the address space.
pub fn map_object(_info: &GateCallInfo, id: ObjID, flags: MapFlags) -> Result<usize, MapError> {
    let slot = twz_rt::OUR_RUNTIME
        .allocate_slot()
        .ok_or(MapError::OutOfResources)?;

    // TODO: track owner

    let Ok(_) = sys_object_map(
        None,
        id,
        slot,
        mapflags_into_prot(flags),
        twizzler_abi::syscall::MapFlags::empty(),
    ) else {
        twz_rt::OUR_RUNTIME.release_slot(slot);
        return Err(MapError::InternalError);
    };

    Ok(slot)
}

/// Unmap an object from the address space.
pub fn unmap_object(_info: &GateCallInfo, slot: usize) {
    // TODO: untrack owner
    if sys_object_unmap(None, slot, UnmapFlags::empty()).is_ok() {
        twz_rt::OUR_RUNTIME.release_slot(slot);
    } else {
        warn!("failed to unmap slot {}", slot);
    }
}
