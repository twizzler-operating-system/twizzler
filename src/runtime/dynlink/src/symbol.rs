pub struct UnrelocatedSymbol {
    _sym: elf::symbol::Symbol,
}
pub struct RelocatedSymbol {
    _sym: elf::symbol::Symbol,
}

pub struct SymbolId(u32);

pub struct SymbolName<'a>(&'a [u8]);

impl<'a> From<&'a str> for SymbolName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value.as_bytes())
    }
}

impl<'a> AsRef<[u8]> for SymbolName<'a> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}

pub trait Symbol {}

impl Symbol for UnrelocatedSymbol {}

impl Symbol for RelocatedSymbol {}

impl From<elf::symbol::Symbol> for UnrelocatedSymbol {
    fn from(value: elf::symbol::Symbol) -> Self {
        Self { _sym: value }
    }
}

impl From<elf::symbol::Symbol> for RelocatedSymbol {
    fn from(value: elf::symbol::Symbol) -> Self {
        Self { _sym: value }
    }
}
