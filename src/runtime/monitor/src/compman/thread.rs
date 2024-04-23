use std::{sync::atomic::AtomicU64, thread::Thread};

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
    threadman::{ManagedThreadRef, DEFAULT_STACK_SIZE, DEFAULT_TLS_ALIGN},
};

use super::{runcomp::RunComp, stack_object::StackObject};

#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct ThreadInitData {
    repr: ObjID,
}

pub(super) struct CompThread {
    stack_object: StackObject,
    thread: ManagedThreadRef,
}

impl CompThread {
    pub fn new(instance: ObjID, start: impl FnOnce() + Send + 'static) -> miette::Result<Self> {
        let thread = crate::threadman::start_managed_thread(start).into_diagnostic()?;
        tracing::info!("==> HERE");
        let init_data = ThreadInitData { repr: thread.id };
        Ok(Self {
            stack_object: StackObject::new(
                instance,
                init_data,
                DEFAULT_TLS_ALIGN,
                DEFAULT_STACK_SIZE,
            )?,
            thread,
        })
    }
}
