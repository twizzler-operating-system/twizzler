use elf::{endian::NativeEndian, ElfBytes, ParseError};

use crate::{
    compartment::CompartmentId,
    symbol::{RelocatedSymbol, Symbol, SymbolName, UnrelocatedSymbol},
    LookupError,
};
mod initialize;
pub(crate) mod internal;
mod load;
mod name;
mod relocate;

pub use load::LibraryLoader;
pub use name::*;

use self::internal::InternalLibrary;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct LibraryId(pub(crate) u64);

pub trait Library {
    type SymbolType: Symbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError>;

    fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError>;

    fn id(&self) -> LibraryId;

    fn compartment_id(&self) -> CompartmentId;

    fn state() -> &'static str;

    fn name(&self) -> &str;

    #[allow(private_interfaces)]
    fn internal(self) -> InternalLibrary;
}

macro_rules! library_state_decl {
    ($name:ident, $sym:ty) => {
        #[derive(Debug, Clone, PartialEq, PartialOrd)]
        pub struct $name {
            int: InternalLibrary,
        }

        impl From<InternalLibrary> for $name {
            fn from(value: InternalLibrary) -> Self {
                Self { int: value }
            }
        }

        impl Library for $name {
            type SymbolType = $sym;

            fn lookup_symbol(&self, name: &SymbolName) -> Result<$sym, LookupError> {
                self.int.lookup_symbol(name)
            }

            fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
                self.int.get_elf()
            }

            fn id(&self) -> LibraryId {
                self.int.id()
            }

            fn compartment_id(&self) -> CompartmentId {
                self.int.compartment_id()
            }

            fn state() -> &'static str {
                stringify!($name)
            }

            fn name(&self) -> &str {
                self.int.name()
            }

            #[allow(private_interfaces)]
            fn internal(self) -> InternalLibrary {
                self.int
            }
        }
    };
}

library_state_decl!(UnloadedLibrary, UnrelocatedSymbol);
library_state_decl!(UnrelocatedLibrary, UnrelocatedSymbol);
library_state_decl!(UninitializedLibrary, RelocatedSymbol);
library_state_decl!(ReadyLibrary, RelocatedSymbol);

pub struct LibraryCollection<L: Library> {
    pub(crate) root: Option<L>,
    pub(crate) deps: Vec<L>,
}

impl<L: Library> From<(L, Vec<L>)> for LibraryCollection<L> {
    fn from(value: (L, Vec<L>)) -> Self {
        Self {
            root: Some(value.0),
            deps: value.1,
        }
    }
}

impl<L: Library> Iterator for LibraryCollection<L> {
    type Item = L;

    fn next(&mut self) -> Option<Self::Item> {
        self.root.take().or_else(|| self.deps.pop())
    }
}

impl<L: Library> From<L> for InternalLibrary {
    fn from(value: L) -> Self {
        value.internal()
    }
}
