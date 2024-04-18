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
    thread::{DEFAULT_STACK_SIZE, SUPER_UPCALL_STACK_SIZE},
};

use super::{runcomp::RunComp, stack_object::StackObject};

pub(super) struct CompThread {
    thread_repr: Option<ObjectHandle>,
    stack_object: StackObject,
}

impl CompThread {
    pub fn new<I: Copy>(rc: &RunComp, init_data: I) -> miette::Result<Self> {
        Ok(Self {
            thread_repr: None,
            stack_object: StackObject::new(rc, init_data, DEFAULT_STACK_SIZE)?,
        })
    }

    fn spawn_thread(&mut self, sctx: ObjID, args: ThreadSpawnArgs) -> Result<ObjID, SpawnError> {
        todo!()
    }

    pub fn start(
        &mut self,
        sctx: ObjID,
        start: extern "C" fn(usize) -> !,
        arg: usize,
    ) -> miette::Result<()> {
        let args = ThreadSpawnArgs {
            stack_size: DEFAULT_STACK_SIZE,
            start: start as *const () as usize,
            arg,
        };
        let id = self.spawn_thread(sctx, args).into_diagnostic()?;

        self.thread_repr = Some(
            twz_rt::OUR_RUNTIME
                .map_object(id, MapFlags::empty())
                .into_diagnostic()?,
        );
        Ok(())
    }
}

unsafe fn do_spawn_thread(
    src_ctx: ObjID,
    args: ThreadSpawnArgs,
    thread_pointer: usize,
    stack_pointer: usize,
    stack_size: usize,
    super_thread_pointer: usize,
    super_stack_pointer: usize,
    super_stack_size: usize,
) -> Result<ObjID, SpawnError> {
    let upcall_target = UpcallTarget::new(
        None,
        Some(twz_rt::rr_upcall_entry),
        super_stack_pointer,
        super_stack_size,
        super_thread_pointer,
        MONITOR_INSTANCE_ID,
        [UpcallOptions {
            flags: UpcallFlags::empty(),
            mode: UpcallMode::CallSuper,
        }; UpcallInfo::NR_UPCALLS],
    );

    sys_spawn(twizzler_abi::syscall::ThreadSpawnArgs {
        entry: args.start,
        stack_base: stack_pointer,
        stack_size: args.stack_size,
        tls: thread_pointer,
        arg: args.arg,
        flags: twizzler_abi::syscall::ThreadSpawnFlags::empty(),
        vm_context_handle: None,
        upcall_target: UpcallTargetSpawnOption::SetTo(upcall_target),
    })
    .map_err(|_| SpawnError::KernelError)
}
