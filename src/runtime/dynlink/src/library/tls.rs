use elf::abi::PT_TLS;
use tracing::debug;

use crate::{compartment::CompartmentRef, tls::TlsModule, DynlinkError};

use super::Library;

impl Library {
    pub(crate) fn get_tls_data(&self) -> Result<Option<&[u8]>, DynlinkError> {
        Ok(self
            .get_elf()?
            .segments()
            .map(|phdrs| phdrs.iter().find(|phdr| phdr.p_type == PT_TLS))
            .flatten()
            .map(|phdr| unsafe {
                if let Some(addr) = self.laddr(phdr.p_vaddr) {
                    Some(core::slice::from_raw_parts(addr, phdr.p_memsz as usize))
                } else {
                    None
                }
            })
            .flatten())
    }

    pub(crate) fn load_tls(&mut self, compartment: &CompartmentRef) -> Result<(), DynlinkError> {
        let data = self.get_tls_data()?;

        if let Some(data) = data.and_then(|data| compartment.make_box_slice(data)) {
            let formatter = humansize::make_format(humansize::BINARY);
            debug!("{}: loading TLS data ({})", self, formatter(data.len()));
            let tm = TlsModule::new_static(data);
            let id = compartment.with_inner_mut(|inner| inner.tls_info.insert(tm))?;
            self.tls_id = Some(id);
        }

        Ok(())
    }
}
