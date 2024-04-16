use miette::IntoDiagnostic;
use twizzler_abi::syscall::{
    sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, ObjectCreate, ObjectCreateError,
    ObjectCreateFlags,
};
use twizzler_runtime_api::{MapFlags, ObjectHandle, ObjectRuntime};

use crate::mapman::MapHandle;

use super::runcomp::RunComp;

pub(crate) struct TlsObject {
    handle: ObjectHandle,
    comphandle: MapHandle,
    stack_size: usize,
    tls_size: usize,
    init_size: usize,
}

impl TlsObject {
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

        let handle = twz_rt::OUR_RUNTIME.map_object(id.as_u128(), MapFlags::empty())?;
        let mh = rc.with_inner(|inner| {
            let mh = inner.map_object(crate::mapman::MapInfo {
                id: id.as_u128(),
                flags: MapFlags::empty(),
            })?;
            Ok(mh.clone())
        })?;

        Ok(Self {
            handle,
            comphandle: mh,
            stack_size,
            tls_size: 0,
            init_size: core::mem::size_of::<T>(),
        })
    }
}
