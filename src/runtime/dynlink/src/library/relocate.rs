use elf::abi::{
    DT_JMPREL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_REL, DT_RELA, DT_RELACOUNT, DT_RELAENT,
    DT_RELASZ, DT_RELCOUNT, DT_RELENT, DT_RELSZ,
};
use tracing::debug;
use twizzler_object::Object;

use crate::{
    compartment::internal::InternalCompartment,
    symbol::{RelocatedSymbol, SymbolName},
    AdvanceError, LookupError,
};

use super::{internal::InternalLibrary, LibraryCollection, UnloadedLibrary, UnrelocatedLibrary};

impl UnrelocatedLibrary {
    pub(crate) fn new(
        old: UnloadedLibrary,
        data: Object<u8>,
        text: Object<u8>,
        deps: Vec<String>,
    ) -> Self {
        let mut next_int = old.int.clone();
        next_int.set_maps(data, text);
        next_int.set_deps(deps);
        Self { int: next_int }
    }
}

impl InternalLibrary {
    pub(crate) fn laddr<T>(&self, _val: u64) -> Option<*const T> {
        todo!()
    }

    #[allow(unused_variables)]
    pub(crate) fn relocate(
        &self,
        _supplemental: Option<&LibraryCollection<UnrelocatedLibrary>>,
        _comp: &InternalCompartment,
        _resolver: &mut SymbolResolver,
    ) -> Result<(), AdvanceError> {
        debug!("relocating library {}", self);
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;
        let dynamic = common
            .dynamic
            .ok_or(AdvanceError::LibraryFailed(self.id()))?;

        let find_dyn_entry = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| self.laddr(d.d_ptr()))
                .flatten()
                .ok_or(AdvanceError::LibraryFailed(self.id()))
        };

        let find_dyn_value = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| d.d_val())
                .ok_or(AdvanceError::LibraryFailed(self.id()))
        };

        let rel: *const u8 = find_dyn_entry(DT_REL)?;
        let rela: *const u8 = find_dyn_entry(DT_RELA)?;
        let jmprel: *const u8 = find_dyn_entry(DT_JMPREL)?;
        let pltgot: *const u8 = find_dyn_entry(DT_PLTGOT)?;
        let pltrel: u64 = find_dyn_value(DT_PLTREL)?;
        let pltrel: u64 = find_dyn_value(DT_PLTRELSZ)?;
        let relacount: u64 = find_dyn_value(DT_RELACOUNT)?;
        let relaent: u64 = find_dyn_value(DT_RELAENT)?;
        let relasz: u64 = find_dyn_value(DT_RELASZ)?;
        let relcount: u64 = find_dyn_value(DT_RELCOUNT)?;
        let relent: u64 = find_dyn_value(DT_RELENT)?;
        let relsz: u64 = find_dyn_value(DT_RELSZ)?;

        todo!()
    }
}

pub struct SymbolResolver {
    lookup: Box<dyn FnMut(SymbolName) -> Result<RelocatedSymbol, LookupError>>,
}

impl SymbolResolver {
    pub fn new(lookup: Box<dyn FnMut(SymbolName) -> Result<RelocatedSymbol, LookupError>>) -> Self {
        Self { lookup }
    }

    pub fn resolve(&mut self, name: SymbolName) -> Result<RelocatedSymbol, LookupError> {
        (self.lookup)(name)
    }
}
