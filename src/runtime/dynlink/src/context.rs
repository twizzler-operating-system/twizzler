use crate::{
    alloc::collections::BTreeMap,
    compartment::{
        CompartmentId, LibraryResolver, ReadyCompartment, UninitializedCompartment,
        UnloadedCompartment, UnrelocatedCompartment,
    },
    library::{LibraryId, LibraryLoader, SymbolResolver},
    symbol::RelocatedSymbol,
    AdvanceError, LookupError,
};

#[derive(Default)]
pub struct Context {
    active_compartments: BTreeMap<CompartmentId, ReadyCompartment>,
    id_counter: u32,
    id_stack: Vec<u32>,

    lib_id_counter: u64,
    lib_id_stack: Vec<u64>,
}

impl Context {
    fn get_fresh_id(&mut self) -> CompartmentId {
        CompartmentId(if let Some(old) = self.id_stack.pop() {
            old
        } else {
            self.id_counter += 1;
            self.id_counter
        })
    }

    pub(crate) fn get_fresh_lib_id(&mut self) -> LibraryId {
        LibraryId(if let Some(old) = self.lib_id_stack.pop() {
            old
        } else {
            self.lib_id_counter += 1;
            self.lib_id_counter
        })
    }

    pub fn new_compartment(&mut self, name: impl ToString) -> UnloadedCompartment {
        UnloadedCompartment::new(name, self.get_fresh_id())
    }

    pub fn lookup_symbol(
        &mut self,
        name: &str,
        primary: CompartmentId,
    ) -> Result<RelocatedSymbol, LookupError> {
        let prim = self.active_compartments.get(&primary);
        if let Some(prim) = prim {
            if let Ok(sym) = prim.internal().lookup_symbol(name) {
                return Ok(sym);
            }
        }

        for (_id, comp) in &self.active_compartments {
            if let Ok(sym) = comp.internal().lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }

    pub fn add_compartment(
        &mut self,
        comp: UnloadedCompartment,
        lib_resolver: &mut LibraryResolver,
        lib_loader: &mut LibraryLoader,
    ) -> Result<CompartmentId, AdvanceError> {
        let id = self.get_fresh_id();
        let loaded = UnrelocatedCompartment::new(comp, self, lib_resolver, lib_loader)?;
        let reloc = UninitializedCompartment::new(loaded, self)?;
        let inited = ReadyCompartment::new(reloc, self)?;
        self.active_compartments.insert(id, inited);
        Ok(id)
    }
}
