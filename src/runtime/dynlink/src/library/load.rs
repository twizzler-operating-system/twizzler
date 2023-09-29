use tracing::{debug, error};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::{ObjectCreateError, ObjectSource},
};
use twizzler_object::{ObjID, Object, ObjectInitError, ObjectInitFlags, Protections};

use crate::{
    compartment::{CompartmentId, LibraryResolver},
    context::Context,
    AdvanceError,
};

use elf::abi::DT_NEEDED;

use super::{internal::InternalLibrary, UnloadedLibrary, UnrelocatedLibrary};

fn within_object(slot: usize, addr: usize) -> bool {
    addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
}

pub struct LibraryLoader {
    create: Box<dyn FnMut(bool, &[ObjectSource]) -> Result<ObjID, ObjectCreateError>>,
    map: Box<dyn FnMut(ObjID, ObjID) -> Result<(Object<u8>, Object<u8>), ObjectInitError>>,
}

impl LibraryLoader {
    pub fn new(
        create: Box<dyn FnMut(bool, &[ObjectSource]) -> Result<ObjID, ObjectCreateError>>,
        map: Box<dyn FnMut(ObjID, ObjID) -> Result<(Object<u8>, Object<u8>), ObjectInitError>>,
    ) -> Self {
        Self { create, map }
    }

    pub fn map(
        &mut self,
        data_id: ObjID,
        text_id: ObjID,
    ) -> Result<(Object<u8>, Object<u8>), ObjectInitError> {
        (self.map)(data_id, text_id)
    }

    pub fn create(
        &mut self,
        data_obj: bool,
        sources: &[ObjectSource],
    ) -> Result<ObjID, ObjectCreateError> {
        (self.create)(data_obj, sources)
    }
}

impl UnloadedLibrary {
    fn enumerate_needed(
        &self,
        resolver: &mut LibraryResolver,
    ) -> Result<Vec<UnloadedLibrary>, AdvanceError> {
        debug!("enumerating needed libraries for {}", self);
        let id = self.internal().id();
        let elf = self
            .internal()
            .get_elf()
            .map_err(|_| AdvanceError::LibraryFailed(id))?;
        let common = elf.find_common_data()?;

        Ok(common
            .dynamic
            .ok_or(AdvanceError::LibraryFailed(id))?
            .iter()
            .filter_map(|d| match d.d_tag {
                DT_NEEDED => Some({
                    let name = common
                        .dynsyms_strs
                        .ok_or(AdvanceError::LibraryFailed(id))
                        .map(|strs| {
                            strs.get(d.d_ptr() as usize)
                                .map_err(|e| AdvanceError::ParseError(e))
                        })
                        .flatten();
                    name.map(|name| {
                        let dep = resolver.resolve(name.into());
                        if dep.is_err() {
                            error!("failed to resolve library {} (needed by {})", name, self);
                        }
                        dep.map_err(|_| AdvanceError::LibraryFailed(id))
                    })
                    .flatten()
                }),
                _ => None,
            })
            .try_collect()?)
    }

    pub fn load(
        self,
        _cxt: &mut Context,
        resolver: &mut LibraryResolver,
        loader: &mut LibraryLoader,
    ) -> Result<(UnrelocatedLibrary, Vec<UnloadedLibrary>), AdvanceError> {
        let elf = self.internal().get_elf()?;
        let id = self.internal().id();
        // TODO: sanity check

        let needed = self.enumerate_needed(resolver)?;
        let deps_list = needed.iter().map(|dep| dep.internal().name().to_string());

        let copy_cmds: Vec<_> = elf
            .segments()
            .ok_or(AdvanceError::LibraryFailed(id))?
            .iter()
            .filter(|p| p.p_type == elf::abi::PT_LOAD)
            .map(|phdr| {
                let targets_data = phdr.p_flags & elf::abi::PF_W != 0;
                let vaddr = phdr.p_vaddr as usize;
                let memsz = phdr.p_memsz as usize;
                let offset = phdr.p_offset as usize;
                let align = phdr.p_align as usize;
                let filesz = phdr.p_filesz as usize;

                if !within_object(if targets_data { 1 } else { 0 }, vaddr)
                    || memsz > MAX_SIZE - NULLPAGE_SIZE * 2
                    || offset > MAX_SIZE - NULLPAGE_SIZE * 2
                    || filesz > memsz
                {
                    return Err(AdvanceError::LibraryFailed(id));
                }

                let src_start = (NULLPAGE_SIZE + offset) & !(align - 1);
                let dest_start = vaddr & !(align - 1);
                let len = (vaddr - dest_start) + filesz;
                Ok((
                    targets_data,
                    twizzler_abi::syscall::ObjectSource::new(
                        self.int.object_id(),
                        src_start as u64,
                        dest_start as u64,
                        len,
                    ),
                ))
            })
            .try_collect()?;

        let text_copy_cmds: Vec<_> = copy_cmds
            .iter()
            .filter(|(td, _)| !*td)
            .map(|(_, c)| c)
            .cloned()
            .collect();
        let data_copy_cmds: Vec<_> = copy_cmds
            .into_iter()
            .filter(|(td, _)| *td)
            .map(|(_, c)| c)
            .collect();

        debug!(
            "creating data object ({} copy commands)",
            data_copy_cmds.len()
        );
        let data_id = loader
            .create(true, &data_copy_cmds)
            .map_err(|_| AdvanceError::LibraryFailed(id))?;
        debug!(
            "creating text object ({} copy commands)",
            data_copy_cmds.len()
        );
        let text_id = loader
            .create(false, &text_copy_cmds)
            .map_err(|_| AdvanceError::LibraryFailed(id))?;

        let (data_object, text_object) = loader
            .map(data_id, text_id)
            .map_err(|_| AdvanceError::LibraryFailed(id))?;

        let unreloc_self =
            UnrelocatedLibrary::new(self, data_object, text_object, deps_list.collect());

        Ok((unreloc_self, needed))
    }

    pub fn new(
        ctx: &mut Context,
        obj_id: ObjID,
        comp_id: CompartmentId,
        name: impl ToString,
    ) -> Result<Self, ObjectInitError> {
        let obj = Object::init_id(obj_id, Protections::READ, ObjectInitFlags::empty())?;
        Ok(Self {
            int: InternalLibrary::new(obj, comp_id, name.to_string(), ctx.get_fresh_lib_id()),
        })
    }
}

impl core::fmt::Display for UnloadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        core::fmt::Display::fmt(&self.int, f)
    }
}
