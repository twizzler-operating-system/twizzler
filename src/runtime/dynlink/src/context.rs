use crate::{
    alloc::collections::BTreeMap,
    compartment::{
        Compartment, CompartmentId, LibraryResolver, ReadyCompartment, UninitializedCompartment,
        UnloadedCompartment,
    },
    symbol::{RelocatedSymbol, SymbolName},
    AdvanceError, LookupError,
};

#[derive(Debug, Default)]
pub struct Context {
    active_compartments: BTreeMap<CompartmentId, ReadyCompartment>,
}

impl Context {
    fn get_fresh_id(&mut self) -> CompartmentId {
        todo!()
    }

    pub fn lookup_symbol(
        &mut self,
        name: &SymbolName,
        primary: CompartmentId,
    ) -> Result<RelocatedSymbol, LookupError> {
        let prim = self.active_compartments.get(&primary);
        if let Some(prim) = prim {
            if let Ok(sym) = prim.lookup_symbol(name) {
                return Ok(sym);
            }
        }

        for (_id, comp) in &self.active_compartments {
            if let Ok(sym) = comp.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }

    pub fn add_compartment(
        &mut self,
        comp: UnloadedCompartment,
        lib_resolver: LibraryResolver,
    ) -> Result<CompartmentId, AdvanceError> {
        let id = self.get_fresh_id();
        let loaded = comp.advance(lib_resolver, self)?;
        let reloc = loaded.advance(self)?;
        let inited = reloc.advance(self)?;
        self.active_compartments.insert(id, inited);
        Ok(id)
    }
}
