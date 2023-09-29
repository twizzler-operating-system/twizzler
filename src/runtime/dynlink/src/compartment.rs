use crate::{
    context::Context,
    library::{
        Library, LibraryId, LibraryLoader, LibraryName, ReadyLibrary, UninitializedLibrary,
        UnloadedLibrary, UnrelocatedLibrary,
    },
    symbol::SymbolName,
    AddLibraryError, AdvanceError, LookupError,
};

mod initialize;
pub(crate) mod internal;
mod load;
mod relocate;

pub trait Compartment {
    type LibraryType: Library;

    fn lookup_symbol(
        &self,
        name: &SymbolName,
    ) -> Result<<<Self as Compartment>::LibraryType as Library>::SymbolType, LookupError>;
    fn id(&self) -> CompartmentId;
}

macro_rules! compartment_state_decl {
    ($name:ident, $lib:ty) => {
        #[derive(Debug, Clone, PartialEq, PartialOrd)]
        pub struct $name {
            int: internal::InternalCompartment,
        }

        impl Compartment for $name {
            type LibraryType = $lib;

            fn lookup_symbol(
                &self,
                name: &SymbolName,
            ) -> Result<<<Self as Compartment>::LibraryType as Library>::SymbolType, LookupError>
            {
                self.int.lookup_symbol(name)
            }

            fn id(&self) -> CompartmentId {
                self.int.id()
            }
        }
    };
}

compartment_state_decl!(UnloadedCompartment, UnloadedLibrary);
compartment_state_decl!(UnrelocatedCompartment, UnrelocatedLibrary);
compartment_state_decl!(UninitializedCompartment, UninitializedLibrary);
compartment_state_decl!(ReadyCompartment, ReadyLibrary);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompartmentId(pub(crate) u32);

pub struct LibraryResolver {
    call: Box<dyn FnMut(LibraryName) -> Result<UnloadedLibrary, LookupError>>,
}

impl LibraryResolver {
    pub fn new(f: Box<dyn FnMut(LibraryName) -> Result<UnloadedLibrary, LookupError>>) -> Self {
        Self { call: f }
    }

    pub fn resolve(&mut self, name: LibraryName) -> Result<UnloadedLibrary, LookupError> {
        (self.call)(name)
    }
}

impl ReadyCompartment {
    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
        resolver: &mut LibraryResolver,
        loader: &mut LibraryLoader,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let coll = self.int.load_library(lib, ctx, resolver, loader)?;
        let coll = self.int.relocate_collection(coll)?;
        let coll = self.int.initialize_collection(coll)?;
        if !self.int.insert_all(coll) {
            return Err(AddLibraryError::AdvanceError(AdvanceError::LibraryFailed(
                id,
            )));
        }
        Ok(id)
    }

    pub(crate) fn new(
        _old: UninitializedCompartment,
        _ctx: &mut Context,
    ) -> Result<ReadyCompartment, AdvanceError> {
        todo!()
    }
}
