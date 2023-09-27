use crate::addr::Address;

pub struct UnrelocatedSymbol {}
pub struct RelocatedSymbol {}

pub struct SymbolId(u32);

pub struct SymbolName<'a>(&'a [u8]);

impl<'a> From<&'a str> for SymbolName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value.as_bytes())
    }
}

pub trait Symbol {
    fn address() -> Address;
}

impl Symbol for UnrelocatedSymbol {
    fn address() -> Address {
        todo!()
    }
}

impl Symbol for RelocatedSymbol {
    fn address() -> Address {
        todo!()
    }
}
