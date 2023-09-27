use elf::{endian::NativeEndian, ElfBytes, ParseError};
use twizzler_object::Object;

use crate::{
    compartment::CompartmentId,
    symbol::{Symbol, SymbolName},
    LookupError,
};

use super::LibraryId;

#[derive(Clone)]
pub(super) struct InternalLibrary {
    object: Object<u8>,
    comp: CompartmentId,
    name: Option<String>,
    id: LibraryId,
}

impl core::fmt::Debug for InternalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalLibrary")
            .field("comp", &self.comp)
            .field("objid", &self.object.id())
            .finish()
    }
}

impl core::fmt::Display for InternalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ref name) = self.name {
            write!(f, "{}", name)
        } else {
            write!(f, "{:?}", self.id)
        }
    }
}

impl PartialOrd for InternalLibrary {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.object.id().partial_cmp(&other.object.id()) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        self.comp.partial_cmp(&other.comp)
    }
}

impl PartialEq for InternalLibrary {
    fn eq(&self, other: &Self) -> bool {
        self.object.id() == other.object.id() && self.comp == other.comp
    }
}

impl InternalLibrary {
    pub(super) fn new(
        object: Object<u8>,
        comp: CompartmentId,
        name: Option<String>,
        id: LibraryId,
    ) -> Self {
        Self {
            object,
            comp,
            name,
            id,
        }
    }

    pub(super) fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        unsafe {
            ElfBytes::minimal_parse(core::slice::from_raw_parts(
                self.object.base_unchecked(),
                0x1000000, // TODO
            ))
        }
    }

    pub(super) fn id(&self) -> LibraryId {
        self.id
    }

    pub(super) fn compartment_id(&self) -> CompartmentId {
        self.comp
    }

    pub(super) fn lookup_symbol<Sym: Symbol + From<elf::symbol::Symbol>>(
        &self,
        name: &SymbolName,
    ) -> Result<Sym, LookupError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

        if let Some(h) = &common.gnu_hash {
            if let Some((_, y)) = h
                .find(
                    name.as_ref(),
                    common.dynsyms.as_ref().ok_or(LookupError::NotFound)?,
                    common.dynsyms_strs.as_ref().ok_or(LookupError::NotFound)?,
                )
                .ok()
                .flatten()
            {
                return Ok(y.into());
            }
        }

        if let Some(h) = &common.sysv_hash {
            if let Some((_, y)) = h
                .find(
                    name.as_ref(),
                    common.dynsyms.as_ref().ok_or(LookupError::NotFound)?,
                    common.dynsyms_strs.as_ref().ok_or(LookupError::NotFound)?,
                )
                .ok()
                .flatten()
            {
                return Ok(y.into());
            }
        }
        Err(LookupError::NotFound)
    }
}
