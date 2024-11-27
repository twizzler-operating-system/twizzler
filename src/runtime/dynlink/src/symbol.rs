//! Definitions for symbols in the dynamic linker.

use crate::library::Library;

/// A (relocated) symbol. Contains information about the symbol itself, like value and size, along
/// with a reference to the library that it comes from.
pub struct RelocatedSymbol<'lib> {
    sym: Option<elf::symbol::Symbol>,
    sc: usize,
    pub(crate) lib: &'lib Library,
}

impl<'lib> RelocatedSymbol<'lib> {
    pub(crate) fn new(sym: elf::symbol::Symbol, lib: &'lib Library) -> Self {
        Self {
            sym: Some(sym),
            lib,
            sc: 0,
        }
    }

    pub(crate) fn new_sc(sc: usize, lib: &'lib Library) -> Self {
        Self { sym: None, lib, sc }
    }

    /// Returns the relocated address of the symbol, i.e. the value of the symbol added to the base
    /// address of the library it comes from.
    pub fn reloc_value(&self) -> u64 {
        self.raw_value() + self.lib.base_addr() as u64
    }

    /// Returns the raw symbol value (unrelocated).
    pub fn raw_value(&self) -> u64 {
        if self.sc > 0 {
            return self.sc as u64;
        }
        self.sym.as_ref().map(|s| s.st_value).unwrap_or_default()
    }

    /// Returns the symbol's size.
    pub fn size(&self) -> u64 {
        self.sym.as_ref().map(|s| s.st_size).unwrap_or_default()
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
        /// Allow any symbols, not just secgates.
        const SKIP_SECGATE_CHECK = 8;
        /// Allow lookup to include weak symbols.
        const ALLOW_WEAK = 0x10;
    }
}
