use std::collections::VecDeque;

use elf::abi::DT_NEEDED;
use tracing::{debug, debug_span};

use crate::{
    alloc::collections::BTreeMap,
    context::Context,
    library::{
        Library, LibraryId, LibraryName, ReadyLibrary, UninitializedLibrary, UnloadedLibrary,
        UnrelocatedLibrary,
    },
    symbol::{RelocatedSymbol, Symbol, SymbolName, UnrelocatedSymbol},
    AddLibraryError, AdvanceError, LookupError,
};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct ReadyCompartment {
    cmp: InternalCompartment<ReadyLibrary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CompartmentId(u32);

#[derive(Default)]
pub struct UnloadedCompartment {
    cmp: InternalCompartment<UnloadedLibrary>,
}

pub struct UnrelocatedCompartment {
    cmp: InternalCompartment<UnrelocatedLibrary>,
}

pub struct UninitializedCompartment {
    cmp: InternalCompartment<UninitializedLibrary>,
}

#[derive(Debug, PartialEq, PartialOrd, Ord, Eq, Clone)]
struct InternalCompartment<L> {
    id: CompartmentId,
    libraries: BTreeMap<LibraryId, L>,
}

impl<L: Library> InternalCompartment<L> {
    fn map<N>(
        self,
        mut f: impl FnMut((LibraryId, L)) -> Result<N, AdvanceError>,
    ) -> Result<InternalCompartment<N>, AdvanceError> {
        Ok(InternalCompartment::<N> {
            libraries: self
                .libraries
                .into_iter()
                .map(|(id, l)| f((id, l)).and_then(|x| Ok((id, x))))
                .try_collect()?,
            id: self.id,
        })
    }

    fn lookup_symbol(&self, name: &SymbolName) -> Result<L::SymbolType, LookupError> where {
        for lib in self.libraries.values() {
            if let Ok(sym) = lib.lookup_symbol(name) {
                return Ok(sym);
            }
        }
        Err(LookupError::NotFound)
    }
}

impl<T> Default for InternalCompartment<T> {
    fn default() -> Self {
        Self {
            libraries: Default::default(),
            id: todo!(),
        }
    }
}

pub trait Compartment {
    type LibraryType: Library;
    type SymbolType: Symbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError>;
}

pub struct LibraryResolver {
    call: Box<dyn FnMut(LibraryName) -> Result<UnloadedLibrary, LookupError>>,
}

impl LibraryResolver {
    pub fn new(f: Box<dyn FnMut(LibraryName) -> Result<UnloadedLibrary, LookupError>>) -> Self {
        Self { call: f }
    }
}

impl Compartment for UnloadedCompartment {
    type LibraryType = UnloadedLibrary;

    type SymbolType = UnrelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError> {
        self.cmp.lookup_symbol(name)
    }
}

impl Compartment for UnrelocatedCompartment {
    type LibraryType = UnrelocatedLibrary;

    type SymbolType = UnrelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError> {
        self.cmp.lookup_symbol(name)
    }
}

impl Compartment for UninitializedCompartment {
    type LibraryType = UninitializedLibrary;

    type SymbolType = RelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError> {
        self.cmp.lookup_symbol(name)
    }
}

impl Compartment for ReadyCompartment {
    type LibraryType = ReadyLibrary;

    type SymbolType = RelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError> {
        self.cmp.lookup_symbol(name)
    }
}

impl UnloadedCompartment {
    pub fn id(&self) -> CompartmentId {
        self.cmp.id
    }

    pub fn advance(
        mut self,
        library_resolver: LibraryResolver,
        ctx: &mut Context,
    ) -> Result<UnrelocatedCompartment, AdvanceError> {
        debug!("advancing compartment {:?}", self.cmp.id);
        let mut next = InternalCompartment::default();

        let mut queue: VecDeque<_> = self.cmp.libraries.into_iter().collect();

        while let Some((id, lib)) = queue.pop_front() {
            debug!("enumerating needed libraries for {:?}", lib);
            let elf = lib.get_elf().map_err(|_| AdvanceError::LibraryFailed(id))?;
            let dynamic = elf
                .dynamic()
                .map_err(|_| AdvanceError::LibraryFailed(id))?
                .ok_or(AdvanceError::LibraryFailed(id))?;

            let neededs = dynamic.iter().filter_map(|d| match d.d_tag {
                DT_NEEDED => Some::<(LibraryId, UnloadedLibrary)>(todo!()),
                _ => None,
            });
            for needed in neededs {
                debug!("adding {} (needed by {})", needed.1, lib);
                queue.push_back(needed);
            }

            next.libraries.insert(id, lib.load(ctx)?);
        }

        Ok(UnrelocatedCompartment { cmp: next })
    }

    pub fn add_library(&mut self, lib: UnloadedLibrary) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        self.cmp.libraries.insert(id, lib);
        Ok(id)
    }
}

impl UnrelocatedCompartment {
    pub fn advance(self, _ctx: &mut Context) -> Result<UninitializedCompartment, AdvanceError> {
        Ok(UninitializedCompartment { cmp: todo!() })
    }

    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let lib = lib.load(ctx)?;
        self.cmp.libraries.insert(id, lib);
        Ok(id)
    }
}

impl UninitializedCompartment {
    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let lib = lib.load(ctx)?;
        let lib = lib.relocate(ctx)?;
        self.cmp.libraries.insert(id, lib);
        Ok(id)
    }

    pub fn advance(self, _ctx: &mut Context) -> Result<ReadyCompartment, AdvanceError> {
        Ok(ReadyCompartment { cmp: todo!() })
    }
}

impl ReadyCompartment {
    pub fn add_library(
        &mut self,
        lib: UnloadedLibrary,
        ctx: &mut Context,
    ) -> Result<LibraryId, AddLibraryError> {
        let id = lib.id();
        let lib = lib.load(ctx)?;
        let lib = lib.relocate(ctx)?;
        let lib = lib.initialize(ctx)?;
        self.cmp.libraries.insert(id, lib);
        Ok(id)
    }
}

impl From<AdvanceError> for AddLibraryError {
    fn from(value: AdvanceError) -> Self {
        Self::AdvanceError(value)
    }
}
