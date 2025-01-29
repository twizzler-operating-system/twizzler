use itertools::{Either, Itertools};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{
        sys_object_create, BackingType, CreateTieFlags, CreateTieSpec, LifetimeType, ObjectCreate,
        ObjectCreateFlags, ObjectSource,
    },
};
use twizzler_object::ObjID;
use twizzler_rt_abi::object::MapFlags;

use super::{Backing, LoadDirective, LoadFlags};
use crate::{DynlinkError, DynlinkErrorKind};

pub struct Engine;

fn within_object(slot: usize, addr: usize) -> bool {
    addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
}

/// Load segments according to Twizzler requirements. Helper function for implementing a
/// ContextEngine.
pub fn load_segments(
    src: &Backing,
    ld: &[LoadDirective],
    instance: ObjID,
) -> Result<Vec<Backing>, DynlinkError> {
    let create_spec = ObjectCreate::new(
        BackingType::Normal,
        LifetimeType::Volatile,
        None,
        ObjectCreateFlags::DELETE,
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

        // NOTE: Data that needs to be initialized to zero is not handled
        // (filesz < memsz). The reason things work now is because
        // the frame allocator in the kernel hands out zeroed pages by default.
        // If this behaviour changes, we will need to explicitly handle it here.
        if directive.filesz != directive.memsz {
            if directive.filesz < directive.memsz {
                // tracing::warn!(
                //     "{} bytes after source implicitly zeroed",
                //     directive.memsz - directive.filesz
                // );
            } else {
                todo!()
            }
        }

        // the offset from the base of the object with the ELF executable data
        let src_start = NULLPAGE_SIZE + directive.offset;
        // the destination offset is the virtual address we want this data
        // to be mapped into. since the different sections are seperated
        // by object boundaries, we keep the object-relative offset
        // we trust the destination offset to be after the NULL_PAGE
        let dest_start = directive.vaddr as usize % MAX_SIZE;
        // the size of the data that must be copied from the ELF
        let len = directive.filesz;

        if !directive.load_flags.contains(LoadFlags::TARGETS_DATA) {
            // Ensure we can direct-map the object for the text directives.
            //
            // The logic for direct mapping between x86_64 and aarch64 is different
            // because the linker/compiler sets the page size to be 64K on aarch64.
            // So we only check if we can direct map for x86_64. The source and
            // destination offsets (for aarch64) would match up if a 64K page size
            // for the NULLPAGE was used or we modified the destination address to
            // be after the NULLPAGE. Loading still works on aarch64, but copies data.
            #[cfg(target_arch = "x86_64")]
            if src_start != dest_start {
                // TODO: check len too.
                return Err(DynlinkError::new(DynlinkErrorKind::LoadDirectiveFail {
                    dir: *directive,
                }));
            }
        }

        Ok(ObjectSource::new_copy(
            src.obj.id(),
            src_start as u64,
            dest_start as u64,
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

    let data_id = sys_object_create(
        create_spec,
        &data_cmds,
        &[CreateTieSpec::new(instance, CreateTieFlags::empty())],
    )
    .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

    let text_id = sys_object_create(
        create_spec,
        &text_cmds,
        &[CreateTieSpec::new(instance, CreateTieFlags::empty())],
    )
    .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

    tracing::info!(
        "mapped segments in instance {} to {}, {}",
        instance,
        text_id,
        data_id
    );

    #[allow(deprecated)]
    let (text_handle, data_handle) = twizzler_rt_abi::object::twz_rt_map_two_objects(
        text_id,
        MapFlags::READ | MapFlags::EXEC,
        data_id,
        MapFlags::READ | MapFlags::WRITE,
    )
    .map_err(|_| DynlinkErrorKind::NewBackingFail)?;

    if data_handle.start() as usize != text_handle.start() as usize + MAX_SIZE {
        tracing::error!(
            "internal runtime error: failed to map text and data adjacent and in-order ({:p} {:p})",
            text_handle.start(),
            data_handle.start()
        );
        return Err(DynlinkErrorKind::NewBackingFail.into());
    }

    Ok(vec![Backing::new(text_handle), Backing::new(data_handle)])
}
