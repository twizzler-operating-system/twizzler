pub struct UnrelocatedSymbol {
    _sym: elf::symbol::Symbol,
}
pub struct RelocatedSymbol {
    sym: elf::symbol::Symbol,
    offset: u64,
}

pub struct SymbolId(u32);

pub trait Symbol {}

impl RelocatedSymbol {
    pub fn new(sym: elf::symbol::Symbol, offset: usize) -> Self {
        Self {
            sym,
            offset: offset as u64,
        }
    }

    pub fn value(&self) -> u64 {
        self.sym.st_value + self.offset
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
