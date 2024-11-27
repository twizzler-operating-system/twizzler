//! Management of individual libraries.

use std::fmt::{Debug, Display};

use elf::{
    abi::{DT_FLAGS_1, PT_PHDR, PT_TLS, STB_WEAK},
    endian::NativeEndian,
    segment::{Elf64_Phdr, ProgramHeader},
    symbol::Symbol,
    ParseError,
};
use petgraph::stable_graph::NodeIndex;
use secgate::RawSecGateInfo;
use twizzler_rt_abi::{
    core::{CtorSet, RuntimeInfo},
    debug::LoadedImageId,
};

use crate::{
    compartment::CompartmentId, engines::Backing, symbol::RelocatedSymbol, tls::TlsModId,
    DynlinkError, DynlinkErrorKind,
};

#[derive(PartialEq, PartialOrd, Ord, Eq, Debug, Clone, Copy)]
pub(crate) enum RelocState {
    /// Relocation has not started.
    Unrelocated,
    /// Relocation has started, but not finished, or failed.
    PartialRelocation,
    /// Relocation completed successfully.
    Relocated,
}

#[repr(C)]
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

impl From<LoadedImageId> for LibraryId {
    fn from(value: LoadedImageId) -> Self {
        LibraryId(NodeIndex::new(value as usize))
    }
}

impl Into<LoadedImageId> for LibraryId {
    fn into(self) -> LoadedImageId {
        self.0.index() as u32
    }
}

impl Display for LibraryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", &self.0.index())
    }
}

#[repr(C)]
/// A loaded library. It may be in various relocation states.
pub struct Library {
    /// Name of this library.
    pub name: String,
    /// Node index for the dependency graph.
    pub(crate) idx: NodeIndex,
    /// Compartment ID this library is loaded in.
    pub(crate) comp_id: CompartmentId,
    /// Just for debug and logging purposes.
    comp_name: String,
    /// Object containing the full ELF data.
    pub full_obj: Backing,
    /// State of relocation.
    pub(crate) reloc_state: RelocState,
    allows_gates: bool,

    pub backings: Vec<Backing>,

    /// The module ID for the TLS region, if any.
    pub tls_id: Option<TlsModId>,

    /// Information about constructors.
    pub(crate) ctors: CtorSet,
    pub(crate) secgate_info: SecgateInfo,
}

#[allow(dead_code)]
impl Library {
    pub(crate) fn new(
        name: String,
        idx: NodeIndex,
        comp_id: CompartmentId,
        comp_name: String,
        full_obj: Backing,
        backings: Vec<Backing>,
        tls_id: Option<TlsModId>,
        ctors: CtorSet,
        secgate_info: SecgateInfo,
        allows_gates: bool,
    ) -> Self {
        Self {
            name,
            idx,
            full_obj,
            backings,
            tls_id,
            ctors,
            reloc_state: RelocState::Unrelocated,
            comp_id,
            comp_name,
            secgate_info,
            allows_gates,
        }
    }
    pub fn allows_gates(&self) -> bool {
        self.allows_gates
    }

    pub fn is_binary(&self) -> bool {
        let Some(dynamic) = self
            .get_elf()
            .ok()
            .and_then(|elf| elf.dynamic().ok())
            .flatten()
        else {
            return false;
        };
        let Some(flags) = dynamic.iter().find_map(|ent| {
            if ent.d_tag == DT_FLAGS_1 {
                Some(ent.d_val())
            } else {
                None
            }
        }) else {
            return false;
        };
        flags & elf::abi::DF_1_PIE as u64 != 0
    }

    /// Get the ID for this library
    pub fn id(&self) -> LibraryId {
        LibraryId(self.idx)
    }

    /// Get the compartment ID for this library.
    pub fn compartment(&self) -> CompartmentId {
        self.comp_id
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

    /// Get a function pointer to this library's entry address, if one exists.
    pub fn get_entry_address(
        &self,
    ) -> Result<extern "C" fn(*const RuntimeInfo) -> !, DynlinkError> {
        let entry = self.get_elf()?.ehdr.e_entry;
        if entry == 0 {
            return Err(DynlinkErrorKind::NoEntryAddress {
                name: self.name.clone(),
            }
            .into());
        }
        let entry: *const u8 = self.laddr(entry);
        let ptr: extern "C" fn(*const RuntimeInfo) -> ! =
            unsafe { core::mem::transmute(entry as usize) };
        Ok(ptr)
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

    fn do_lookup_symbol(
        &self,
        name: &str,
        allow_weak: bool,
    ) -> Result<RelocatedSymbol<'_>, DynlinkError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;
        tracing::info!("lookup {} in {}", name, self.name);

        /*
        if self.is_relocated() {
            if let Some(gates) = self.iter_secgates() {
                for sc in gates {
                    if let Ok(gname) = sc.name().to_str() {
                        if gname == name {
                            tracing::info!("found as secure gate");
                            return Ok(RelocatedSymbol::new_sc(sc.imp, self));
                        }
                    }
                }
            }
        }
        */

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
                    tracing::info!(
                        "==> {}: {} {}",
                        name,
                        sym.st_bind() == STB_WEAK,
                        self.is_relocated() && self.is_secgate(name)
                    );
                    // TODO: proper weak symbol handling.
                    if sym.st_bind() != STB_WEAK
                        || allow_weak
                        || (self.is_relocated() && self.is_secgate(name))
                    {
                        return Ok(RelocatedSymbol::new(sym, self));
                    } else {
                        tracing::debug!("lookup symbol {} skipping weak binding in {}", name, self);
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
                    if sym.st_bind() != STB_WEAK
                        || allow_weak
                        || (self.is_relocated() && self.is_secgate(name))
                    {
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

    pub(crate) fn lookup_symbol(
        &self,
        name: &str,
        allow_weak: bool,
        allow_prefix: bool,
    ) -> Result<RelocatedSymbol<'_>, DynlinkError> {
        let ret = self.do_lookup_symbol(&name, allow_weak);
        if allow_prefix && ret.is_err() && !name.starts_with("__TWIZZLER_SECURE_GATE_") {
            let name = format!("__TWIZZLER_SECURE_GATE_{}", name);
            tracing::info!("trying with prefix: {}", name);
            if let Ok(o) = self.do_lookup_symbol(&name, allow_weak) {
                return Ok(o);
            }
        }
        ret
    }

    pub(crate) fn is_local_or_secgate_from(&self, other: &Library, name: &str) -> bool {
        self.in_same_compartment_as(other) || (self.is_relocated() && self.is_secgate(name))
    }

    pub(crate) fn in_same_compartment_as(&self, other: &Library) -> bool {
        other.comp_id == self.comp_id
    }

    pub fn is_relocated(&self) -> bool {
        self.reloc_state == RelocState::Relocated
    }

    fn is_secgate(&self, name: &str) -> bool {
        self.iter_secgates()
            .map(|gates| {
                gates
                    .iter()
                    .any(|gate| gate.name().to_bytes() == name.as_bytes())
            })
            .unwrap_or(false)
    }

    pub fn iter_secgates(&self) -> Option<&[RawSecGateInfo]> {
        let addr = self.secgate_info.info_addr?;

        Some(unsafe { core::slice::from_raw_parts(addr as *const _, self.secgate_info.num) })
    }
}

impl Debug for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Library")
            .field("name", &self.name)
            .field("comp_name", &self.comp_name)
            .field("idx", &self.idx)
            .field("tls_id", &self.tls_id)
            .finish()
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        tracing::debug!("dynlink: drop library: {:?}", self);
    }
}

impl core::fmt::Display for Library {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}::{}", &self.comp_name, &self.name)
    }
}

impl core::fmt::Display for UnloadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}(unloaded)", &self.name)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SecgateInfo {
    pub info_addr: Option<usize>,
    pub num: usize,
}
