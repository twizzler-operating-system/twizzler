use elf::{endian::NativeEndian, ElfBytes, ParseError};
use twizzler_object::{ObjID, Object};

use crate::{
    compartment::CompartmentId,
    symbol::{Symbol, SymbolName},
    LookupError,
};

use super::LibraryId;

#[derive(Clone)]
pub(crate) struct InternalLibrary {
    object: Object<u8>,
    comp: CompartmentId,
    name: String,
    id: LibraryId,
    deps_list: Vec<String>,
    text_map: Option<Object<u8>>,
    data_map: Option<Object<u8>>,
}

impl Ord for InternalLibrary {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.partial_cmp(other).unwrap()
    }
}

impl Eq for InternalLibrary {}

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
        write!(f, "{}", self.name)
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
        name: String,
        id: LibraryId,
    ) -> Self {
        Self {
            object,
            comp,
            name,
            id,
            deps_list: vec![],
            text_map: None,
            data_map: None,
        }
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(super) fn set_maps(&mut self, data: Object<u8>, text: Object<u8>) {
        assert!(self.text_map.is_none());
        assert!(self.data_map.is_none());
        self.text_map = Some(text);
        self.data_map = Some(data);
    }

    pub(super) fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        unsafe {
            ElfBytes::minimal_parse(core::slice::from_raw_parts(
                self.object.base_unchecked(),
                0x1000000, // TODO
            ))
        }
    }

    pub(crate) fn id(&self) -> LibraryId {
        self.id
    }

    pub(super) fn object_id(&self) -> ObjID {
        self.object.id()
    }

    pub(crate) fn lookup_symbol<Sym: Symbol + From<elf::symbol::Symbol>>(
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

    pub(crate) fn set_deps(&mut self, deps_list: Vec<String>) {
        self.deps_list = deps_list;
    }
}
