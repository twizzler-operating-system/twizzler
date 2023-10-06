use crate::library::LibraryRef;

pub struct UnrelocatedSymbol {
    _sym: elf::symbol::Symbol,
}
pub struct RelocatedSymbol {
    sym: elf::symbol::Symbol,
    pub(crate) lib: LibraryRef,
}

pub struct SymbolId(u32);

pub trait Symbol {}

impl RelocatedSymbol {
    pub fn new(sym: elf::symbol::Symbol, lib: LibraryRef) -> Self {
        Self { sym, lib }
    }

    pub fn reloc_value(&self) -> u64 {
        self.sym.st_value + self.lib.base_addr.unwrap() as u64
    }

    pub fn raw_value(&self) -> u64 {
        self.sym.st_value
    }

    pub fn size(&self) -> u64 {
        self.sym.st_size
    }
}

impl Symbol for UnrelocatedSymbol {}

impl Symbol for RelocatedSymbol {}

impl From<elf::symbol::Symbol> for UnrelocatedSymbol {
    fn from(value: elf::symbol::Symbol) -> Self {
        Self { _sym: value }
    }
}
