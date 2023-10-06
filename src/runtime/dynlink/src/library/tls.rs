use elf::{abi::PT_TLS, segment::ProgramHeader};
use tracing::debug;

use crate::{compartment::CompartmentRef, tls::TlsModule, DynlinkError};

use super::Library;

#[allow(dead_code)]
impl Library {
    fn get_tls_phdr(&self) -> Result<Option<ProgramHeader>, DynlinkError> {
        Ok(self
            .get_elf()?
            .segments()
            .map(|phdrs| phdrs.iter().find(|phdr| phdr.p_type == PT_TLS))
            .flatten())
    }

    pub(crate) fn get_tls_data(&self) -> Result<Option<&[u8]>, DynlinkError> {
        Ok(self
            .get_tls_phdr()?
            .map(|phdr| unsafe {
                if let Some(addr) = self.laddr(phdr.p_vaddr) {
                    Some(core::slice::from_raw_parts(addr, phdr.p_memsz as usize))
                } else {
                    None
                }
            })
            .flatten())
    }

    pub(crate) fn register_tls(
        &mut self,
        compartment: &CompartmentRef,
    ) -> Result<(), DynlinkError> {
        let phdr = self.get_tls_phdr()?;

        if let Some(phdr) = phdr {
            let formatter = humansize::make_format(humansize::BINARY);
            debug!(
                "{}: registering TLS data ({} total, {} copy)",
                self,
                formatter(phdr.p_memsz),
                formatter(phdr.p_filesz)
            );
            let tm = TlsModule::new_static(
                self.laddr::<u8>(phdr.p_vaddr).unwrap() as usize,
                phdr.p_filesz as usize,
                phdr.p_memsz as usize,
                phdr.p_align as usize,
            );
            let id = compartment.with_inner_mut(|inner| inner.tls_info.insert(tm))?;
            self.tls_id = Some(id);
        }

        Ok(())
    }
}
