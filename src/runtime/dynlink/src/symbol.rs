//! Definitions for symbols in the dynamic linker.

use crate::library::{BackingData, Library};

/// A (relocated) symbol. Contains information about the symbol itself, like value and size, along with a reference to
/// the library that it comes from.
pub struct RelocatedSymbol<'lib, Backing: BackingData> {
    sym: elf::symbol::Symbol,
    pub(crate) lib: &'lib Library<Backing>,
}

impl<'lib, Backing: BackingData> RelocatedSymbol<'lib, Backing> {
    pub(crate) fn new(sym: elf::symbol::Symbol, lib: &'lib Library<Backing>) -> Self {
        Self { sym, lib }
    }

    /// Returns the relocated address of the symbol, i.e. the value of the symbol added to the base address of the library it comes from.
    pub fn reloc_value(&self) -> u64 {
        self.sym.st_value + self.lib.base_addr() as u64
    }

    /// Returns the raw symbol value (unrelocated).
    pub fn raw_value(&self) -> u64 {
        self.sym.st_value
    }

    /// Returns the symbol's size.
    pub fn size(&self) -> u64 {
        self.sym.st_size
    }
}

bitflags::bitflags! {
    #[derive(Copy, Clone)]
    /// Options for use during symbol lookup. If all of these flags are specified together, the search will fail.
    pub struct LookupFlags : u32 {
        /// Look elsewhere first. Note that the symbol may still bind to us if the dep graph has a cycle.
        const SKIP_SELF = 1;
        /// Don't look through dependencies, go straight to global search.
        const SKIP_DEPS = 2;
        /// Don't do a global search.
        const SKIP_GLOBAL = 4;
    }
}
