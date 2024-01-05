use itertools::{Either, Itertools};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, LifetimeType, ObjectCreate, ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_runtime_api::MapFlags;

use crate::{
    context::engine::{ContextEngine, LoadDirective, LoadFlags},
    library::BackingData,
    DynlinkError, DynlinkErrorKind,
};

#[derive(Clone)]
pub struct Backing {
    obj: twizzler_runtime_api::ObjectHandle,
}

impl Backing {
    pub fn new(inner: twizzler_runtime_api::ObjectHandle) -> Self {
        Self { obj: inner }
    }
}

impl BackingData for Backing {
    fn data(&self) -> (*mut u8, usize) {
        (
            unsafe { self.obj.start.add(NULLPAGE_SIZE) },
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }

    fn new_data() -> Result<Self, DynlinkError> {
        let runtime = twizzler_runtime_api::get_runtime();
        let id = sys_object_create(
            ObjectCreate::new(
                BackingType::Normal,
                LifetimeType::Volatile,
                None,
                ObjectCreateFlags::empty(),
            ),
            &[],
            &[],
        )
        .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        let handle = runtime
            .map_object(id.as_u128(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        Ok(Self { obj: handle })
    }

    fn load_addr(&self) -> usize {
        self.obj.start as usize
    }

    type InnerType = twizzler_runtime_api::ObjectHandle;

    fn to_inner(self) -> Self::InnerType {
        self.obj
    }
}

pub struct Engine;

fn within_object(slot: usize, addr: usize) -> bool {
    addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
}

impl ContextEngine for Engine {
    type Backing = Backing;

    fn load_segments(
        &mut self,
        src: &Self::Backing,
        ld: &[crate::context::engine::LoadDirective],
    ) -> Result<Vec<Self::Backing>, DynlinkError> {
        let create_spec = ObjectCreate::new(
            BackingType::Normal,
            LifetimeType::Volatile,
            None,
            ObjectCreateFlags::empty(),
        );

        let build_copy_cmd = |directive: &LoadDirective| {
            if !within_object(
                if directive.load_flags.contains(LoadFlags::TARGETS_DATA) {
                    1
                } else {
                    0
                },
                directive.vaddr,
            ) || directive.memsz > MAX_SIZE - NULLPAGE_SIZE * 2
                || directive.offset > MAX_SIZE - NULLPAGE_SIZE * 2
                || directive.filesz > directive.memsz
            {
                return Err(DynlinkError::new(DynlinkErrorKind::LoadDirectiveFail {
                    dir: *directive,
                }));
            }

            if directive.filesz != directive.memsz {
                todo!()
            }

            let src_start = (NULLPAGE_SIZE + directive.offset) & !(directive.align - 1);
            let dest_start = directive.vaddr & !(directive.align - 1);
            let len = (directive.vaddr - dest_start) + directive.filesz;

            Ok(ObjectSource::new_copy(
                twizzler_object::ObjID::new(src.obj.id),
                (src_start % MAX_SIZE) as u64,
                (dest_start % MAX_SIZE) as u64,
                len,
            ))
        };

        let ld = ld.into_iter().cloned().collect::<Vec<_>>();
        let (data_cmds, text_cmds): (Vec<_>, Vec<_>) = ld.into_iter().partition_map(|directive| {
            if directive.load_flags.contains(LoadFlags::TARGETS_DATA) {
                Either::Left(build_copy_cmd(&directive))
            } else {
                Either::Right(build_copy_cmd(&directive))
            }
        });

        let data_cmds = DynlinkError::collect(DynlinkErrorKind::NewBackingFail, data_cmds)?;
        let text_cmds = DynlinkError::collect(DynlinkErrorKind::NewBackingFail, text_cmds)?;

        let data_id = sys_object_create(create_spec, &data_cmds, &[])
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        let text_id = sys_object_create(create_spec, &text_cmds, &[])
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

        let runtime = twizzler_runtime_api::get_runtime();
        let text_handle = runtime
            .map_object(text_id.as_u128(), MapFlags::READ | MapFlags::EXEC)
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;
        let data_handle = runtime
            .map_object(data_id.as_u128(), MapFlags::READ | MapFlags::WRITE)
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

        Ok(vec![Backing::new(text_handle), Backing::new(data_handle)])
    }
}
