use std::fmt::{Debug, Display};

use elf::{endian::NativeEndian, CommonElfData, ElfBytes, ParseError};
use twizzler_abi::object::ObjID;
use twizzler_object::{Object, ObjectInitError, ObjectInitFlags, Protections};

use crate::{
    compartment::CompartmentId,
    context::Context,
    symbol::{RelocatedSymbol, Symbol, SymbolName, UnrelocatedSymbol},
    AdvanceError, LookupError,
};

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct ReadyLibrary {
    int: InternalLibrary,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq)]
pub struct LibraryId(ObjID);

#[derive(Clone)]
struct InternalLibrary {
    object: Object<u8>,
    comp: CompartmentId,
    name: Option<String>,
    id: LibraryId,
}

impl core::fmt::Debug for InternalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InternalLibrary")
            .field("comp", &self.comp)
            .field("objid", &self.object.id())
            .finish()
    }
}

impl Display for InternalLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ref name) = self.name {
            write!(f, "{}", name)
        } else {
            write!(f, "{:?}", self.id)
        }
    }
}

impl PartialOrd for InternalLibrary {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        match self.object.id().partial_cmp(&other.object.id()) {
            Some(core::cmp::Ordering::Equal) => {}
            ord => return ord,
        }
        self.comp.partial_cmp(&other.comp)
    }
}

impl PartialEq for InternalLibrary {
    fn eq(&self, other: &Self) -> bool {
        self.object.id() == other.object.id() && self.comp == other.comp
    }
}

impl InternalLibrary {
    fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        unsafe {
            ElfBytes::minimal_parse(core::slice::from_raw_parts(
                self.object.base_unchecked(),
                0x1000000, // TODO
            ))
        }
    }

    fn lookup_symbol<Sym: Symbol + From<elf::symbol::Symbol>>(
        &self,
        name: &SymbolName,
    ) -> Result<Sym, LookupError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;
        let sym = basic_lookup(name, &common).ok_or(LookupError::NotFound)?;
        Ok(sym.into())
    }
}

#[derive(Clone, Debug, PartialEq, PartialOrd)]
pub struct UnloadedLibrary {
    int: InternalLibrary,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct UnrelocatedLibrary {
    int: InternalLibrary,
}

#[derive(Debug, Clone, PartialEq, PartialOrd)]
pub struct UninitializedLibrary {
    int: InternalLibrary,
}

pub trait Library {
    type SymbolType: Symbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<Self::SymbolType, LookupError>;
}

impl Library for ReadyLibrary {
    type SymbolType = RelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<RelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}

impl Library for UninitializedLibrary {
    type SymbolType = RelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<RelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}

impl Library for UnrelocatedLibrary {
    type SymbolType = UnrelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<UnrelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}

fn basic_lookup(
    name: &SymbolName,
    common: &CommonElfData<'_, NativeEndian>,
) -> Option<elf::symbol::Symbol> {
    if let Some(h) = &common.gnu_hash {
        if let Some((_, y)) = h
            .find(
                name.as_ref(),
                common.dynsyms.as_ref()?,
                common.dynsyms_strs.as_ref()?,
            )
            .ok()
            .flatten()
        {
            return Some(y);
        }
    }

    if let Some(h) = &common.sysv_hash {
        if let Some((_, y)) = h
            .find(
                name.as_ref(),
                common.dynsyms.as_ref()?,
                common.dynsyms_strs.as_ref()?,
            )
            .ok()
            .flatten()
        {
            return Some(y);
        }
    }
    None
}

impl Library for UnloadedLibrary {
    type SymbolType = UnrelocatedSymbol;

    fn lookup_symbol(&self, name: &SymbolName) -> Result<UnrelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}

impl From<ParseError> for LookupError {
    fn from(value: ParseError) -> Self {
        LookupError::ParseError(value)
    }
}

pub struct LibraryName<'a>(&'a [u8]);

impl<'a> From<&'a str> for LibraryName<'a> {
    fn from(value: &'a str) -> Self {
        Self(value.as_bytes())
    }
}

impl UnloadedLibrary {
    pub fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        self.int.get_elf()
    }

    pub fn id(&self) -> LibraryId {
        self.int.id
    }

    pub fn load(&self, _cxt: &mut Context) -> Result<UnrelocatedLibrary, AdvanceError> {
        todo!()
    }

    pub fn new(
        id: ObjID,
        comp_id: CompartmentId,
        name: impl ToString,
    ) -> Result<Self, ObjectInitError> {
        let obj = Object::init_id(id, Protections::READ, ObjectInitFlags::empty())?;
        Ok(Self {
            int: InternalLibrary {
                object: obj,
                comp: comp_id,
                name: Some(name.to_string()),
                id: LibraryId(id),
            },
        })
    }
}

impl Display for UnloadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.int, f)
    }
}

impl UnrelocatedLibrary {
    pub fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        self.int.get_elf()
    }

    pub fn relocate(self, _ctx: &mut Context) -> Result<UninitializedLibrary, AdvanceError> {
        todo!()
    }

    pub fn lookup_symbol(&self, name: &SymbolName) -> Result<UnrelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}

impl UninitializedLibrary {
    pub fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        self.int.get_elf()
    }

    pub fn initialize(self, _ctx: &mut Context) -> Result<ReadyLibrary, AdvanceError> {
        todo!()
    }
}

impl ReadyLibrary {
    pub fn get_elf(&self) -> Result<ElfBytes<'_, NativeEndian>, ParseError> {
        self.int.get_elf()
    }

    pub fn lookup_symbol(&self, name: &SymbolName) -> Result<RelocatedSymbol, LookupError> {
        self.int.lookup_symbol(name)
    }
}
