//! Management of individual libraries.

use std::{
    fmt::{Debug, Display},
    sync::{
        atomic::{AtomicU8, Ordering},
        OnceLock,
    },
};

use elf::{
    abi::{DT_FLAGS_1, PT_DYNAMIC, PT_PHDR, PT_TLS, STB_WEAK},
    dynamic::Dyn,
    endian::NativeEndian,
    segment::{Elf64_Phdr, ProgramHeader},
    ParseError,
};
use petgraph::stable_graph::NodeIndex;
use secgate::RawSecGateInfo;
use smallstr::SmallString;
use twizzler_abi::object::ObjID;
use twizzler_rt_abi::{
    core::{CtorSet, RuntimeInfo},
    debug::LoadedImageId,
};

use crate::{
    compartment::CompartmentId, engines::Backing, symbol::RelocatedSymbol, tls::TlsModId,
    DynlinkError, DynlinkErrorKind,
};

#[derive(PartialEq, PartialOrd, Ord, Eq, Debug, Clone, Copy)]
#[repr(u8)]
pub(crate) enum RelocState {
    /// Relocation has not started.
    Unrelocated = 0,
    /// Relocation has started, but not finished, or failed.
    PartialRelocation = 1,
    /// Relocation completed successfully.
    Relocated = 2,
}

/// Relocation state, written through a shared reference.
///
/// Relocation runs under a dynlink *read* lock, so a library can be present in the graph while
/// another thread is still relocating it. Readers must gate on [`Library::is_relocated`] before
/// calling into a library; the release/acquire pair here is what makes the relocation writes
/// visible to a reader that observes `Relocated`.
#[repr(transparent)]
pub(crate) struct AtomicRelocState(AtomicU8);

impl AtomicRelocState {
    fn new(state: RelocState) -> Self {
        Self(AtomicU8::new(state as u8))
    }

    pub(crate) fn get(&self) -> RelocState {
        match self.0.load(Ordering::Acquire) {
            0 => RelocState::Unrelocated,
            1 => RelocState::PartialRelocation,
            _ => RelocState::Relocated,
        }
    }

    pub(crate) fn set(&self, state: RelocState) {
        self.0.store(state as u8, Ordering::Release);
    }
}

impl Debug for AtomicRelocState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self.get(), f)
    }
}

#[derive(PartialEq, PartialOrd, Ord, Eq, Debug, Clone, Copy)]
pub enum AllowedGates {
    /// Gates are not exported
    Private,
    /// Gates are exported to other compartments only
    Public,
    /// Gates are exported to all compartments
    PublicInclSelf,
}

/// The file ranges an ELF image's PT_LOAD segments occupy, as `(offset, len)` byte pairs.
///
/// Parsed straight off a mapped image, with no `Context` and no `Library`: the monitor calls this
/// to work out what to prefault *before* it takes the dynlink write lock. The loadable part of a
/// release-built DSO here is under 10% of the file -- the rest is DWARF -- so a whole-object
/// preload would read an order of magnitude more than the load touches.
pub fn pt_load_ranges(data: &[u8]) -> Result<std::vec::Vec<(u64, u64)>, ParseError> {
    let elf = elf::ElfBytes::<NativeEndian>::minimal_parse(data)?;
    Ok(elf
        .segments()
        .map(|phdrs| {
            phdrs
                .iter()
                .filter(|p| p.p_type == elf::abi::PT_LOAD && p.p_filesz > 0)
                .map(|p| (p.p_offset, p.p_filesz))
                .collect()
        })
        .unwrap_or_default())
}

#[repr(C)]
/// An unloaded library. It's just a name, really.
#[derive(Debug, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
pub struct UnloadedLibrary {
    pub name: String,
    pub id: Option<ObjID>,
}

impl UnloadedLibrary {
    /// Construct a new unloaded library.
    pub fn new(name: impl AsRef<str>) -> Self {
        Self {
            name: name.as_ref().to_string(),
            id: None,
        }
    }

    pub fn new_object(name: impl AsRef<str>, id: ObjID) -> Self {
        Self {
            name: name.as_ref().to_string(),
            id: Some(id),
        }
    }
}

/// The ID struct for a library.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Ord, Eq, Hash, Default)]
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
    pub(crate) reloc_state: AtomicRelocState,
    allowed_gates: AllowedGates,

    pub backings: Vec<Backing>,

    /// The module ID for the TLS region, if any.
    pub tls_id: Option<TlsModId>,

    /// Information about constructors.
    pub(crate) ctors: CtorSet,
    pub(crate) secgate_info: SecgateInfo,

    // Caching stuff
    elf: OnceLock<elf::ElfBytes<'static, NativeEndian>>,
    elf_common: OnceLock<elf::CommonElfData<'static, NativeEndian>>,
    /// Gate names from `.twz_secgate_info`, built once. is_secgate() is called for every candidate
    /// library on every cross-compartment symbol lookup, and used to be a linear scan with a
    /// byte-compare per gate.
    secgate_names: OnceLock<std::collections::HashSet<Box<str>>>,
    /// Which names this library's ELF object defines, approximately. Shared with every other
    /// compartment that loaded the same object; see [crate::context::SymBloom].
    pub(crate) sym_bloom: Option<std::sync::Arc<crate::context::SymBloom>>,
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
        allowed_gates: AllowedGates,
    ) -> Self {
        Self {
            name,
            idx,
            full_obj,
            backings,
            tls_id,
            ctors,
            reloc_state: AtomicRelocState::new(RelocState::Unrelocated),
            comp_id,
            comp_name,
            secgate_info,
            allowed_gates,
            elf: OnceLock::new(),
            elf_common: OnceLock::new(),
            secgate_names: OnceLock::new(),
            sym_bloom: None,
        }
    }

    pub fn allows_gates(&self) -> bool {
        self.allowed_gates != AllowedGates::Private
    }

    pub fn allows_self_gates(&self) -> bool {
        self.allowed_gates == AllowedGates::PublicInclSelf
    }

    pub fn dynamic_ptr(&self) -> Option<*mut Dyn> {
        let phdr = self
            .get_elf()
            .ok()?
            .segments()?
            .iter()
            .find(|s| s.p_type == PT_DYNAMIC)?;
        Some(self.laddr_mut(phdr.p_vaddr))
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
    pub fn get_elf(&self) -> Result<&elf::ElfBytes<'static, NativeEndian>, ParseError> {
        self.elf.get_or_try_init(|| unsafe {
            elf::ElfBytes::<'static, NativeEndian>::minimal_parse(std::mem::transmute(
                self.full_obj.slice(),
            ))
        })
    }

    /// Return a handle to the full ELF file.
    pub fn get_elf_common(&self) -> Result<&elf::CommonElfData<'static, NativeEndian>, ParseError> {
        self.elf_common
            .get_or_try_init(|| self.get_elf().and_then(|e| e.find_common_data()))
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
                name: self.name.as_str().into(),
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
        let common = self.get_elf_common()?;

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
                            name: "dynsyms".into(),
                        })?,
                    common.dynsyms_strs.as_ref().ok_or_else(|| {
                        DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".into(),
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
                        tracing::warn!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    //tracing::warn!("undefined symbol: {}", name);
                    return Err(DynlinkErrorKind::NameNotFound { name: name.into() }.into());
                }
            }
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
                            name: "dynsyms".into(),
                        })?,
                    common.dynsyms_strs.as_ref().ok_or_else(|| {
                        DynlinkErrorKind::MissingSection {
                            name: "dynsyms_strs".into(),
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
                        tracing::warn!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                } else {
                    //tracing::warn!("undefined symbol: {}", name);
                }
            }
        }
        /*
        if !self.allows_gates()
            && !self.allows_self_gates()
            && self.is_binary()
            && !name.starts_with("__TWIZZLER_SECURE_GATE")
            && false
        {
            tracing::warn!("trying gate lookup");
            let dstrs = common.dynsyms_strs.as_ref().unwrap();
            for sym in common.dynsyms.as_ref().unwrap().iter() {
                let sym_name = dstrs.get(sym.st_name as usize)?;
                if name == sym_name {
                    if sym.st_bind() == STB_WEAK && allow_weak && !self.is_secgate(name) {
                        /*
                        tracing::warn!(
                            "!! lookup symbol {} skipping weak binding in {}",
                            name,
                            self
                        );
                        */
                        return Ok(RelocatedSymbol::new_zero(self));
                    } else {
                        //tracing::warn!("lookup symbol {} skipping weak binding in {}", name,
                        // self);
                    }
                }
            }
        }
        */
        //tracing::warn!("undefined symbol: {}", name);
        Err(DynlinkErrorKind::NameNotFound { name: name.into() }.into())
    }

    pub(crate) fn lookup_symbol(
        &self,
        name: &str,
        allow_weak: bool,
        allow_prefix: bool,
    ) -> Result<RelocatedSymbol<'_>, DynlinkError> {
        let ret = self.do_lookup_symbol(&name, allow_weak);
        // A `__TWIZZLER_SECURE_GATE_*` symbol only exists in a library that has gates, so skip the
        // string build and the second hash probe entirely for the libraries that have none (which
        // is most of them: libstd, libc, libtwz_rt). Previously this constructed a 256-byte
        // SmallString and probed the hash table for every candidate library on every lookup.
        if allow_prefix
            && ret.is_err()
            && self.secgate_info.num > 0
            && !name.starts_with("__TWIZZLER_SECURE_GATE_")
        {
            let mut prefixedname = SmallString::<[u8; 256]>::from_str("__TWIZZLER_SECURE_GATE_");
            prefixedname.push_str(name);

            if let Ok(o) = self.do_lookup_symbol(&prefixedname, allow_weak) {
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
        self.reloc_state.get() == RelocState::Relocated
    }

    fn is_secgate(&self, name: &str) -> bool {
        // Fast reject before touching the set: most libraries export no gates at all.
        if self.secgate_info.num == 0 {
            return false;
        }
        let build = || {
            let _start = std::time::Instant::now();
            let set: std::collections::HashSet<Box<str>> = self
                .iter_secgates()
                .map(|gates| {
                    gates
                        .iter()
                        .filter_map(|gate| gate.name().to_str().ok())
                        .map(Box::from)
                        .collect()
                })
                .unwrap_or_default();
            secgate::statlog::record_on_anon(
                crate::context::SGNAME_STATS,
                "SGNAMES",
                _start.elapsed().as_nanos() as u64 / 1000,
                &[set.len() as u64],
            );
            set
        };
        self.secgate_names.get_or_init(build).contains(name)
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
        //tracing::warn!("dynlink: drop library: {:?}", self);
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
