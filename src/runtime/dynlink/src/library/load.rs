use std::sync::Arc;

use tracing::{debug, error};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::ObjectSource,
};
use twizzler_object::Object;

use elf::abi::DT_NEEDED;

use crate::{context::ContextInner, DynlinkError, ECollector};

use super::{Library, LibraryRef};

fn within_object(slot: usize, addr: usize) -> bool {
    addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
}

pub trait LibraryLoader {
    fn create_segments(
        &mut self,
        text: &[ObjectSource],
        data: &[ObjectSource],
    ) -> Result<(Object<u8>, Object<u8>), DynlinkError>;

    fn open(&mut self, name: &str) -> Result<Object<u8>, DynlinkError>;
}

impl Library {
    pub(crate) fn enumerate_needed(
        &self,
        loader: &mut impl LibraryLoader,
    ) -> Result<Vec<Library>, DynlinkError> {
        debug!("enumerating needed libraries for {}", self);
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

        Ok(common
            .dynamic
            .ok_or(DynlinkError::Unknown)?
            .iter()
            .filter_map(|d| match d.d_tag {
                DT_NEEDED => Some({
                    let name = common
                        .dynsyms_strs
                        .ok_or(DynlinkError::Unknown)
                        .map(|strs| {
                            strs.get(d.d_ptr() as usize)
                                .map_err(|_| DynlinkError::Unknown)
                        })
                        .flatten();
                    name.map(|name| {
                        let dep = loader.open(name.into());
                        if dep.is_err() {
                            error!("failed to resolve library {} (needed by {})", name, self);
                        }
                        dep.map(|dep| dep.into())
                    })
                    .flatten()
                }),
                _ => None,
            })
            .ecollect()?)
    }

    pub(crate) fn load(
        &mut self,
        _cxt: &mut ContextInner,
        loader: &mut impl LibraryLoader,
    ) -> Result<(), DynlinkError> {
        let elf = self.get_elf()?;
        // TODO: sanity check

        let copy_cmds: Vec<_> = elf
            .segments()
            .ok_or(DynlinkError::Unknown)?
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
                    return Err(DynlinkError::Unknown);
                }

                let src_start = (NULLPAGE_SIZE + offset) & !(align - 1);
                let dest_start = vaddr & !(align - 1);
                let len = (vaddr - dest_start) + filesz;
                Ok((
                    targets_data,
                    twizzler_abi::syscall::ObjectSource::new(
                        todo!(),
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
            "creating data ({} copy commands) and text ({} copy commands)",
            data_copy_cmds.len(),
            text_copy_cmds.len()
        );
        let (data_obj, text_obj) = loader.create_segments(&data_copy_cmds, &text_copy_cmds)?;
        Ok(())
    }
}
