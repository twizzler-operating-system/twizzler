use twizzler_runtime_api::{
    MapFlags, ObjID, ObjectHandle, ObjectRuntime, SpawnError, ThreadSpawnArgs,
};

use miette::IntoDiagnostic;

use crate::{mapman::MapHandle, thread::DEFAULT_STACK_SIZE};

pub(super) struct CompThread {
    thread_repr: Option<ObjectHandle>,
    tls_object: MapHandle,
}

impl CompThread {
    pub fn new(tls: MapHandle) -> Self {
        Self {
            thread_repr: None,
            tls_object: tls,
        }
    }

    fn spawn_thread(&mut self, sctx: ObjID, args: ThreadSpawnArgs) -> Result<ObjID, SpawnError> {
        let tp = todo!();
        let sp = todo!();
        crate::thread::spawn_thread(sctx, args, tp, sp)
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
