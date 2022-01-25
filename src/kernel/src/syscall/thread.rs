use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSpawnArgs, ThreadSpawnError},
};

pub fn sys_spawn(_args: &ThreadSpawnArgs) -> Result<ObjID, ThreadSpawnError> {
    todo!()
}
