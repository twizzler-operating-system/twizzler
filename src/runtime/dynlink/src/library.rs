//! Management of individual libraries.

use std::fmt::Debug;

use elf::{
    abi::{PT_PHDR, PT_TLS, STB_WEAK},
    endian::NativeEndian,
    segment::{Elf64_Phdr, ProgramHeader},
    ParseError,
};

use petgraph::stable_graph::NodeIndex;
use twizzler_abi::object::MAX_SIZE;

use crate::{symbol::RelocatedSymbol, tls::TlsModId, DynlinkError};

/// State of relocation.
#[derive(Debug)]
#[repr(u32)]
pub(crate) enum RelocState {
    /// The library has not been relocated.
    Unrelocated,
    /// The library is currently being relocated.
    Relocating,
    /// The library is relocated.
    Relocated,
}

#[allow(dead_code)]
#[derive(Debug)]
#[repr(u32)]
pub(crate) enum InitState {
    /// No constructors have been called.
    Uninit,
    /// This library has been loaded as part of the static set, but hasn't been initialized (waiting for runtime entry).
    StaticUninit,
    /// Constructors have been called, destructors have not been called.
    Constructed,
    /// Destructors have been called.
    Deconstructed,
}

pub trait BackingData {
    fn data(&self) -> (*mut u8, usize);
    fn new_data() -> Self;
    fn load_addr(&self) -> usize;
}

pub struct UnloadedLibrary {
    pub name: String,
}

pub struct Library<Backing: BackingData> {
    /// Name of this library.
    pub name: String,
    /// Node index for the dependency graph.
    pub(crate) idx: NodeIndex,
    /// Object containing the full ELF data.
    pub full_obj: Backing,
    /// State of relocation (see [RelocState]).
    reloc_state: RelocState,
    /// State of initialization (see [InitState]).
    init_state: InitState,

    pub backings: Vec<Backing>,

    /// The module ID for the TLS region, if any.
    pub tls_id: Option<TlsModId>,

    /// Information about constructors, if any.
    pub(crate) ctors: Option<CtorInfo>,
}

#[allow(dead_code)]
impl<Backing: BackingData> Library<Backing> {
    pub fn new(
        name: String,
        idx: NodeIndex,
        full_obj: Backing,
        backings: Vec<Backing>,
        tls_id: Option<TlsModId>,
        ctors: Option<CtorInfo>,
    ) -> Self {
        Self {
            name,
            idx,
            full_obj,
            reloc_state: RelocState::Unrelocated,
            init_state: InitState::Uninit,
            backings,
            tls_id,
            ctors,
        }
    }

    pub fn get_phdrs_raw(&self) -> Option<(*const Elf64_Phdr, usize)> {
        Some((
            self.get_elf().ok()?.segments()?.iter().find_map(|p| {
                if p.p_type == PT_PHDR {
                    Some(self.laddr(p.p_vaddr))
                } else {
                    None
                }
            })??,
            self.get_elf().ok()?.segments()?.len(),
        ))
    }

    /// Return a handle to the full ELF file.
    pub fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        let slice =
            unsafe { core::slice::from_raw_parts(self.full_obj.base_unchecked(), MAX_SIZE) };
        elf::ElfBytes::minimal_parse(slice)
    }

    pub fn base_addr(&self) -> usize {
        self.backings[0].load_addr()
    }

    pub fn laddr<T>(&self, val: u64) -> *const T {
        (self.base_addr() + val as usize) as *const T
    }

    pub fn laddr_mut<T>(&self, val: u64) -> *mut T {
        (self.base_addr() + val as usize) as *mut T
    }

    // Helper to find the TLS program header.
    fn get_tls_phdr(&self) -> Result<Option<ProgramHeader>, DynlinkError> {
        Ok(self
            .get_elf()?
            .segments()
            .and_then(|phdrs| phdrs.iter().find(|phdr| phdr.p_type == PT_TLS)))
    }

    pub(crate) fn get_tls_data(&self) -> Result<Option<&[u8]>, DynlinkError> {
        Ok(self.get_tls_phdr()?.and_then(|phdr| unsafe {
            self.laddr(phdr.p_vaddr)
                .map(|addr| core::slice::from_raw_parts(addr, phdr.p_memsz as usize))
        }))
    }

    pub(crate) fn lookup_symbol(&self, name: &str) -> Result<RelocatedSymbol, DynlinkError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

        // Try the GNU hash table, if present.
        if let Some(h) = &common.gnu_hash {
            if let Some((_, sym)) = h
                .find(
                    name.as_ref(),
                    common.dynsyms.as_ref().ok_or(DynlinkError::Unknown)?,
                    common.dynsyms_strs.as_ref().ok_or(DynlinkError::Unknown)?,
                )
                .ok()
                .flatten()
            {
                if !sym.is_undefined() {
                    // TODO: proper weak symbol handling.
                    if sym.st_bind() != STB_WEAK {
                        return Ok(RelocatedSymbol::new(sym, self.clone()));
                    } else {
                        tracing::info!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    tracing::info!("undefined symbol: {}", name);
                }
            }
            return Err(self.error(crate::DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            }));
        }

        // Try the sysv hash table, if present.
        if let Some(h) = &common.sysv_hash {
            if let Some((_, sym)) = h
                .find(
                    name.as_ref(),
                    common.dynsyms.as_ref().ok_or(DynlinkError::Unknown)?,
                    common.dynsyms_strs.as_ref().ok_or(DynlinkError::Unknown)?,
                )
                .ok()
                .flatten()
            {
                if !sym.is_undefined() {
                    // TODO: proper weak symbol handling.
                    if sym.st_bind() != STB_WEAK {
                        return Ok(RelocatedSymbol::new(sym, self.clone()));
                    } else {
                        tracing::info!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    tracing::info!("undefined symbol: {}", name);
                }
            }
        }
        Err(self.error(crate::DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }))
    }
}

impl<B: BackingData> Debug for Library<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("name", &self.name)
            .field("comp_id", &self.comp_id)
            .finish()
    }
}

impl<B: BackingData> core::fmt::Display for Library<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.name)
    }
}

#[derive(Debug, Clone, Copy)]
pub struct CtorInfo {
    pub legacy_init: usize,
    pub init_array: usize,
    pub init_array_len: usize,
}
