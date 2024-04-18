use miette::IntoDiagnostic;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, ObjectCreate, ObjectCreateFlags,
};
use twizzler_runtime_api::{MapError, MapFlags, ObjectHandle, ObjectRuntime};

use crate::mapman::MapHandle;

use super::runcomp::RunComp;

pub(crate) struct StackObject {
    handle: ObjectHandle,
    comphandle: MapHandle,
    stack_size: usize,
    init_size: usize,
}

impl StackObject {
    pub fn new<T>(rc: &RunComp, init_data: T, stack_size: usize) -> miette::Result<Self> {
        let cs = ObjectCreate::new(
            BackingType::Normal,
            twizzler_abi::syscall::LifetimeType::Volatile,
            Some(rc.instance.into()),
            ObjectCreateFlags::empty(),
        );
        let id = sys_object_create(
            cs,
            &[],
            &[CreateTieSpec::new(
                rc.instance.into(),
                CreateTieFlags::empty(),
            )],
        )
        .into_diagnostic()?;

        let handle = twz_rt::OUR_RUNTIME
            .map_object(id, MapFlags::empty())
            .into_diagnostic()?;
        let mh = rc
            .with_inner(|inner| {
                let mh = inner.map_object(crate::mapman::MapInfo {
                    id,
                    flags: MapFlags::empty(),
                })?;
                Ok::<_, MapError>(mh.clone())
            })
            .into_diagnostic()?;

        Ok(Self {
            handle,
            comphandle: mh,
            stack_size,
            init_size: core::mem::size_of::<T>(),
        })
    }

    pub fn stack_comp_start(&self) -> usize {
        self.comphandle.addrs().start
    }

    pub fn stack_size(&self) -> usize {
        self.stack_size
    }

    pub fn init_data_comp_start(&self) -> usize {
        self.stack_comp_start() + self.stack_comp_start()
    }

    pub fn init_data_size(&self) -> usize {
        self.init_size
    }
}
