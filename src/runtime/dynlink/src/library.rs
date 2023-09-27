use elf::{endian::NativeEndian, ElfBytes, ParseError};
use twizzler_abi::object::ObjID;

use crate::{
    compartment::CompartmentId,
    symbol::{RelocatedSymbol, Symbol, SymbolName, UnrelocatedSymbol},
    LookupError,
};
mod initialize;
mod internal;
mod load;
mod name;
mod relocate;

pub use name::*;

use self::internal::InternalLibrary;

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct LibraryId(ObjID);

pub trait Library {
    type SymbolType: Symbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError>;

    fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError>;

    fn id(&self) -> LibraryId;

    fn compartment_id(&self) -> CompartmentId;

    fn state() -> &'static str;
}

macro_rules! library_state_decl {
    ($name:ident, $sym:ty) => {
        #[derive(Debug, Clone, PartialEq, PartialOrd)]
        pub struct $name {
            int: InternalLibrary,
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
        }
    };
}

library_state_decl!(UnloadedLibrary, UnrelocatedSymbol);
library_state_decl!(UnrelocatedLibrary, UnrelocatedSymbol);
library_state_decl!(UninitializedLibrary, RelocatedSymbol);
library_state_decl!(ReadyLibrary, RelocatedSymbol);
