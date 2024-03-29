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
            // one null-page, one meta-page
            MAX_SIZE - NULLPAGE_SIZE * 2,
        )
    }

    fn load_addr(&self) -> usize {
        self.obj.start as usize
    }

    type InnerType = twizzler_runtime_api::ObjectHandle;

    fn to_inner(self) -> Self::InnerType {
        self.obj
    }

    fn inner(&self) -> &Self::InnerType {
        &self.obj
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

            if !directive.load_flags.contains(LoadFlags::TARGETS_DATA) {
                // Ensure we can direct-map the object for the text directives.
                if src_start != dest_start || directive.filesz != directive.memsz {
                    // TODO: check len too.
                    return Err(DynlinkError::new(DynlinkErrorKind::LoadDirectiveFail {
                        dir: *directive,
                    }));
                }
            }

            Ok(ObjectSource::new_copy(
                src.obj.id,
                (src_start % MAX_SIZE) as u64,
                (dest_start % MAX_SIZE) as u64,
                len,
            ))
        };

        let ld = ld.to_vec();
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

        let (text_handle, data_handle) = runtime
            .map_two_objects(
                text_id.as_u128(),
                MapFlags::READ | MapFlags::EXEC,
                data_id,
                MapFlags::READ | MapFlags::WRITE,
            )
            .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

        if data_handle.start as usize != text_handle.start as usize + MAX_SIZE {
            tracing::error!("internal runtime error: failed to map text and data adjacent and in-order ({:p} {:p})", text_handle.start, data_handle.start);
            return Err(DynlinkErrorKind::NewBackingFail.into());
        }

        Ok(vec![Backing::new(text_handle), Backing::new(data_handle)])
    }
}
