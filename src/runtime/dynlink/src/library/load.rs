use tracing::{debug, trace};
use twizzler_abi::{
    object::{MAX_SIZE, NULLPAGE_SIZE},
    syscall::ObjectSource,
};
use twizzler_object::Object;

use crate::{context::ContextInner, DynlinkError};

use super::Library;

fn within_object(slot: usize, addr: usize) -> bool {
    addr >= slot * MAX_SIZE + NULLPAGE_SIZE && addr < (slot + 1) * MAX_SIZE - NULLPAGE_SIZE * 2
}

pub trait LibraryLoader {
    fn create_segments(
        &mut self,
        data: &[ObjectSource],
        text: &[ObjectSource],
    ) -> Result<(Object<u8>, Object<u8>), DynlinkError>;

    fn open(&mut self, name: &str) -> Result<Object<u8>, DynlinkError>;
}

impl Library {
    // Load (map) a single library into memory via creating two objects, one for text, and one for data.
    pub(crate) fn load(
        &mut self,
        _cxt: &mut ContextInner,
        loader: &mut impl LibraryLoader,
    ) -> Result<(), DynlinkError> {
        let elf = self.get_elf()?;
        // TODO: sanity check

        // Step 1: map the PT_LOAD directives to copy-from commands Twizzler can use for creating objects.
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

                trace!(
                    "{}: load directive: vaddr={:x}, memsz={:x}, offset={:x}, filesz={:x}",
                    self,
                    vaddr,
                    memsz,
                    offset,
                    filesz
                );
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
                        self.full_obj.id(),
                        src_start as u64,
                        dest_start as u64,
                        len,
                    ),
                ))
            })
            .try_collect()?;

        // Separate out the commands for text and data segmets.
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
            "{}: creating data ({} copy commands) and text ({} copy commands)",
            self,
            data_copy_cmds.len(),
            text_copy_cmds.len()
        );
        let (data_obj, text_obj) = loader.create_segments(&data_copy_cmds, &text_copy_cmds)?;
        // The base address is the "0-point" for the virtual addresses within the library.
        let base_addr = unsafe { text_obj.base_unchecked() as *const _ as usize } - NULLPAGE_SIZE;
        unsafe {
            debug!(
                "{}: loaded: text = {:p}, data = {:p}, base = {:x}",
                self,
                text_obj.base_unchecked(),
                data_obj.base_unchecked(),
                base_addr
            );
        }
        self.set_mapping(data_obj, text_obj, base_addr);

        Ok(())
    }
}
