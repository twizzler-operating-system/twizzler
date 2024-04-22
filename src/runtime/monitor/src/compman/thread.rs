use twizzler_abi::{
    syscall::{sys_spawn, UpcallTargetSpawnOption},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_runtime_api::{
    MapFlags, ObjID, ObjectHandle, ObjectRuntime, SpawnError, ThreadSpawnArgs,
};

use miette::IntoDiagnostic;

use crate::{
    api::MONITOR_INSTANCE_ID,
    threadman::{ManagedThreadRef, DEFAULT_STACK_SIZE},
};

use super::{runcomp::RunComp, stack_object::StackObject};

pub(super) struct CompThread {
    stack_object: StackObject,
}

impl CompThread {
    pub fn new<I: Copy>(rc: &RunComp, init_data: I) -> miette::Result<Self> {
        Ok(Self {
            stack_object: StackObject::new(rc, init_data, DEFAULT_STACK_SIZE)?,
        })
    }
}
