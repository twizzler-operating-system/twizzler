use crate::{
    alloc::collections::BTreeMap,
    compartment::{
        Compartment, CompartmentId, LibraryResolver, ReadyCompartment, UninitializedCompartment,
        UnloadedCompartment,
    },
    AdvanceError,
};

#[derive(Debug, Default)]
pub struct Context {
    active_compartments: BTreeMap<CompartmentId, ReadyCompartment>,
}

impl Context {
    fn get_fresh_id(&mut self) -> CompartmentId {
        todo!()
    }

    pub fn add_compartment(
        &mut self,
        comp: UnloadedCompartment,
    ) -> Result<CompartmentId, AdvanceError> {
        let id = self.get_fresh_id();
        let loaded = comp.advance(LibraryResolver::new(Box::new(|_name| todo!())), self)?;
        let reloc = loaded.advance(self)?;
        let inited = reloc.advance(self)?;
        self.active_compartments.insert(id, inited);
        todo!()
    }
}
