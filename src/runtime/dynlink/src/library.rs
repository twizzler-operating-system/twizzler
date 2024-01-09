//! Management of individual libraries.

use std::fmt::{Debug, Display};

use elf::{
    abi::{PT_PHDR, PT_TLS, STB_WEAK},
    endian::NativeEndian,
    segment::{Elf64_Phdr, ProgramHeader},
    ParseError,
};

use petgraph::stable_graph::NodeIndex;
use secgate::RawSecGateInfo;

use crate::{symbol::RelocatedSymbol, tls::TlsModId, DynlinkError, DynlinkErrorKind};

pub(crate) enum RelocState {
    /// Relocation has not started.
    Unrelocated,
    /// Relocation has started, but not finished, or failed.
    PartialRelocation,
    /// Relocation completed successfully.
    Relocated,
}

/// The core trait that represents loaded or mapped data.
pub trait BackingData: Clone {
    /// Get a pointer to the start of a region, and a length, denoting valid memory representing this object. The memory
    /// region is valid.
    fn data(&self) -> (*mut u8, usize);

    /// Make a new backing data for holding allocated data for the dynamic linker.
    fn new_data() -> Result<Self, DynlinkError>
    where
        Self: Sized;
    fn load_addr(&self) -> usize;

    /// Get the data as a slice.
    fn slice(&self) -> &[u8] {
        let data = self.data();
        // Safety: a loaded library may have a slice constructed of its backing data.
        unsafe { core::slice::from_raw_parts(data.0, data.1) }
    }

    type InnerType;
    /// Get the inner implementation type.
    fn to_inner(self) -> Self::InnerType;

    /// Get the ELF file for this backing.
    fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        elf::ElfBytes::minimal_parse(self.slice())
    }
}

/// An unloaded library. It's just a name, really.
#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct UnloadedLibrary {
    pub name: String,
}

impl UnloadedLibrary {
    /// Construct a new unloaded library.
    pub fn new(name: impl ToString) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

/// The ID struct for a library.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash)]
#[repr(transparent)]
pub struct LibraryId(pub(crate) NodeIndex);

impl Display for LibraryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.0.index())
    }
}

/// A loaded library. It may be in various relocation states.
pub struct Library<Backing: BackingData> {
    /// Name of this library.
    pub name: String,
    /// Node index for the dependency graph.
    pub(crate) idx: NodeIndex,
    /// Compartment name this library is loaded in.
    pub(crate) comp_name: String,
    /// Object containing the full ELF data.
    pub full_obj: Backing,
    /// State of relocation.
    pub(crate) reloc_state: RelocState,

    pub backings: Vec<Backing>,

    /// The module ID for the TLS region, if any.
    pub tls_id: Option<TlsModId>,

    /// Information about constructors.
    pub(crate) ctors: CtorInfo,
    pub(crate) secgate_info: SecgateInfo,
}

#[allow(dead_code)]
impl<Backing: BackingData> Library<Backing> {
    pub(crate) fn new(
        name: String,
        idx: NodeIndex,
        comp_name: String,
        full_obj: Backing,
        backings: Vec<Backing>,
        tls_id: Option<TlsModId>,
        ctors: CtorInfo,
        secgate_info: SecgateInfo,
    ) -> Self {
        Self {
            name,
            idx,
            full_obj,
            backings,
            tls_id,
            ctors,
            reloc_state: RelocState::Unrelocated,
            comp_name,
            secgate_info,
        }
    }

    /// Get the ID for this library
    pub fn id(&self) -> LibraryId {
        LibraryId(self.idx)
    }

    /// Get a raw pointer to the program headers for this library.
    pub fn get_phdrs_raw(&self) -> Option<(*const Elf64_Phdr, usize)> {
        Some((
            self.get_elf().ok()?.segments()?.iter().find_map(|p| {
                if p.p_type == PT_PHDR {
                    Some(self.laddr(p.p_vaddr))
                } else {
                    None
                }
            })?,
            self.get_elf().ok()?.segments()?.len(),
        ))
    }

    /// Return a handle to the full ELF file.
    pub fn get_elf(&self) -> Result<elf::ElfBytes<'_, NativeEndian>, ParseError> {
        elf::ElfBytes::minimal_parse(self.full_obj.slice())
    }

    /// Get the load address for this library.
    pub fn base_addr(&self) -> usize {
        self.backings[0].load_addr()
    }

    /// Compute an in-memory address for a ELF virtual addr.
    pub fn laddr<T>(&self, val: u64) -> *const T {
        (self.base_addr() + val as usize) as *const T
    }

    /// Compute an in-memory address (mut) for a ELF virtual addr.
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
        let phdr = self.get_tls_phdr()?;
        Ok(phdr.map(|phdr| {
            let slice = unsafe {
                core::slice::from_raw_parts(self.laddr(phdr.p_vaddr), phdr.p_memsz as usize)
            };
            slice
        }))
    }

    pub(crate) fn lookup_symbol(
        &self,
        name: &str,
    ) -> Result<RelocatedSymbol<'_, Backing>, DynlinkError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

        // Try the GNU hash table, if present.
        if let Some(h) = &common.gnu_hash {
            if let Some((_, sym)) = h
                .find(
                    name.as_ref(),
                    common
                        .dynsyms
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::MissingSection {
                            name: "dynsyms".to_string(),
                        })?,
                    common.dynsyms_strs.as_ref().ok_or_else(|| {
                        DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".to_string(),
                        }
                    })?,
                )
                .ok()
                .flatten()
            {
                if !sym.is_undefined() {
                    // TODO: proper weak symbol handling.
                    if sym.st_bind() != STB_WEAK {
                        return Ok(RelocatedSymbol::new(sym, self));
                    } else {
                        tracing::info!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    tracing::info!("undefined symbol: {}", name);
                }
            }
            return Err(DynlinkErrorKind::NameNotFound {
                name: name.to_string(),
            }
            .into());
        }

        // Try the sysv hash table, if present.
        if let Some(h) = &common.sysv_hash {
            if let Some((_, sym)) = h
                .find(
                    name.as_ref(),
                    common
                        .dynsyms
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::MissingSection {
                            name: "dynsyms".to_string(),
                        })?,
                    common.dynsyms_strs.as_ref().ok_or_else(|| {
                        DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".to_string(),
                        }
                    })?,
                )
                .ok()
                .flatten()
            {
                if !sym.is_undefined() {
                    // TODO: proper weak symbol handling.
                    if sym.st_bind() != STB_WEAK {
                        return Ok(RelocatedSymbol::new(sym, self));
                    } else {
                        tracing::info!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    tracing::info!("undefined symbol: {}", name);
                }
            }
        }
        Err(DynlinkErrorKind::NameNotFound {
            name: name.to_string(),
        }
        .into())
    }

    pub fn iter_secgates(&self) -> Option<&[RawSecGateInfo]> {
        let addr = self.secgate_info.info_addr?;

        Some(unsafe { core::slice::from_raw_parts(addr as *const _, self.secgate_info.num) })
    }
}

impl<B: BackingData> Debug for Library<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("name", &self.name)
            .field("comp_name", &self.comp_name)
            .field("idx", &self.idx)
            .field("tls_id", &self.tls_id)
            .finish()
    }
}

impl<B: BackingData> core::fmt::Display for Library<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", &self.comp_name, &self.name)
    }
}

impl core::fmt::Display for UnloadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}(unloaded)", &self.name)
    }
}

/// Information about constructors for a library.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CtorInfo {
    /// Legacy pointer to _init function for a library. Can be called with the C abi.
    pub legacy_init: usize,
    /// Pointer to start of the init array, which contains functions pointers that can be called by the C abi.
    pub init_array: usize,
    /// Length of the init array.
    pub init_array_len: usize,
}

#[derive(Debug, Clone, Default)]
pub struct SecgateInfo {
    pub info_addr: Option<usize>,
    pub num: usize,
}
