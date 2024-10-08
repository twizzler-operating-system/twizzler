use std::{collections::HashSet, mem::size_of};

use elf::{
    abi::{
        DF_TEXTREL, DT_FLAGS, DT_FLAGS_1, DT_JMPREL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_REL,
        DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELENT, DT_RELSZ,
    },
    endian::NativeEndian,
    parse::{ParseAt, ParsingIterator},
    relocation::{Rel, Rela},
    string_table::StringTable,
    symbol::SymbolTable,
};
use tracing::{debug, error, trace};

use super::{Context, Library};
use crate::{
    arch::{REL_DTPMOD, REL_DTPOFF, REL_GOT, REL_PLT, REL_RELATIVE, REL_SYMBOLIC, REL_TPOFF},
    library::{LibraryId, RelocState},
    symbol::LookupFlags,
    DynlinkError, DynlinkErrorKind,
};

// A relocation is either a REL type or a RELA type. The only difference is that
// the RELA type contains an addend (used in the reloc calculations below).
#[derive(Debug)]
enum EitherRel {
    Rel(Rel),
    Rela(Rela),
}

impl EitherRel {
    fn r_type(&self) -> u32 {
        match self {
            EitherRel::Rel(r) => r.r_type,
            EitherRel::Rela(r) => r.r_type,
        }
    }

    fn addend(&self) -> i64 {
        match self {
            EitherRel::Rel(_) => 0,
            EitherRel::Rela(r) => r.r_addend,
        }
    }

    fn offset(&self) -> u64 {
        match self {
            EitherRel::Rel(r) => r.r_offset,
            EitherRel::Rela(r) => r.r_offset,
        }
    }

    fn sym(&self) -> u32 {
        match self {
            EitherRel::Rel(r) => r.r_sym,
            EitherRel::Rela(r) => r.r_sym,
        }
    }
}

impl Context {
    pub(crate) fn get_parsing_iter<P: ParseAt>(
        &self,
        start: *const u8,
        ent: usize,
        sz: usize,
    ) -> Option<ParsingIterator<'_, NativeEndian, P>> {
        P::validate_entsize(elf::file::Class::ELF64, ent).ok()?;
        let iter = ParsingIterator::new(NativeEndian, elf::file::Class::ELF64, unsafe {
            core::slice::from_raw_parts(start, sz)
        });
        Some(iter)
    }

    fn do_reloc(
        &self,
        lib: &Library,
        rel: EitherRel,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
    ) -> Result<(), DynlinkError> {
        let addend = rel.addend();
        let base = lib.base_addr() as u64;
        let target: *mut u64 = lib.laddr_mut(rel.offset());
        // Lookup a symbol if the relocation's symbol index is non-zero.
        let symbol = if rel.sym() != 0 {
            let sym = syms.get(rel.sym() as usize)?;
            let flags = LookupFlags::empty();
            strings
                .get(sym.st_name as usize)
                .map(|name| (name, self.lookup_symbol(lib.id(), name, flags)))
                .ok()
        } else {
            None
        };

        // Helper for logging errors.
        let open_sym = || {
            if let Some((name, sym)) = symbol {
                if let Ok(sym) = sym {
                    trace!(
                        "{}: found symbol {} at {:x} from {}",
                        lib,
                        name,
                        sym.reloc_value(),
                        sym.lib
                    );
                    Result::<_, DynlinkError>::Ok(sym)
                } else {
                    error!("{}: needed symbol {} not found", lib, name);
                    Err(DynlinkErrorKind::SymbolLookupFail {
                        symname: name.to_string(),
                        sourcelib: lib.name.to_string(),
                    }
                    .into())
                }
            } else {
                error!("{}: invalid relocation, no symbol data", lib);
                Err(DynlinkErrorKind::MissingSection {
                    name: "symbol data".to_string(),
                }
                .into())
            }
        };

        // This is where the magic happens.
        match rel.r_type() {
            REL_RELATIVE => unsafe { *target = base.wrapping_add_signed(addend) },
            REL_SYMBOLIC => unsafe {
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend)
            },
            REL_PLT | REL_GOT => unsafe { *target = open_sym()?.reloc_value() },
            REL_DTPMOD => {
                // See the TLS module for understanding where the TLS ID is coming from.
                let id = if rel.sym() == 0 {
                    lib.tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: lib.name.clone(),
                        })?
                        .tls_id()
                } else {
                    let other_lib = open_sym()?.lib;
                    other_lib
                        .tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: other_lib.name.clone(),
                        })?
                        .tls_id()
                };
                unsafe { *target = id }
            }
            REL_DTPOFF => {
                let val = open_sym().map(|sym| sym.raw_value()).unwrap_or(0);
                unsafe { *target = val.wrapping_add_signed(addend) }
            }
            REL_TPOFF => {
                if let Some(tls) = lib.tls_id {
                    let val = open_sym().map(|sym| sym.raw_value()).unwrap_or(0);
                    unsafe {
                        *target = val
                            .wrapping_sub(tls.offset() as u64)
                            .wrapping_add_signed(addend)
                    }
                } else {
                    error!("{}: TPOFF relocations require a PT_TLS segment", lib);
                    Err(DynlinkErrorKind::NoTLSInfo {
                        library: lib.name.clone(),
                    })?
                }
            }
            _ => {
                error!("{}: unsupported relocation: {}", lib, rel.r_type());
                Result::<_, DynlinkError>::Err(
                    DynlinkErrorKind::UnsupportedReloc {
                        library: lib.name.clone(),
                        reloc: rel.r_type().to_string(),
                    }
                    .into(),
                )?
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_rels(
        &self,
        lib: &Library,
        start: *const u8,
        ent: usize,
        sz: usize,
        name: &str,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
    ) -> Result<(), DynlinkError> {
        debug!(
            "{}: processing {} relocations (num = {})",
            lib,
            name,
            sz / ent
        );
        // Try to parse the table as REL or RELA, according to ent size. If get_parsing_iter
        // succeeds for a given relocation type, that's the correct one.
        if let Some(rels) = self.get_parsing_iter(start, ent, sz) {
            DynlinkError::collect(
                DynlinkErrorKind::RelocationSectionFail {
                    secname: "REL".to_string(),
                    library: lib.name.clone(),
                },
                rels.map(|rel| self.do_reloc(lib, EitherRel::Rel(rel), strings, syms)),
            )?;
            Ok(())
        } else if let Some(relas) = self.get_parsing_iter(start, ent, sz) {
            DynlinkError::collect(
                DynlinkErrorKind::RelocationSectionFail {
                    secname: "RELA".to_string(),
                    library: lib.name.clone(),
                },
                relas.map(|rela| self.do_reloc(lib, EitherRel::Rela(rela), strings, syms)),
            )?;
            Ok(())
        } else {
            let info = format!("reloc '{}' with entsz {}, size {}", name, ent, sz);
            Err(DynlinkErrorKind::UnsupportedReloc {
                library: lib.name.clone(),
                reloc: info,
            }
            .into())
        }
    }

    pub(crate) fn relocate_single(&mut self, lib_id: LibraryId) -> Result<(), DynlinkError> {
        let lib = self.get_library(lib_id)?;
        debug!("{}: relocating library", lib);
        let elf = lib.get_elf()?;
        let common = elf.find_common_data()?;
        let dynamic = common
            .dynamic
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynamic".to_string(),
            })?;

        // Helper to lookup a single entry for a relocated pointer in the dynamic table.
        let find_dyn_entry = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| lib.laddr(d.d_ptr()))
        };

        // Helper to lookup a single value in the dynamic table.
        let find_dyn_value = |tag| dynamic.iter().find(|d| d.d_tag == tag).map(|d| d.d_val());

        // Many of the relocation tables are described in a similar way -- start, entry size, and
        // table size (in bytes).
        let find_dyn_rels = |tag, ent, sz| {
            let rel = find_dyn_entry(tag);
            let relent = find_dyn_value(ent);
            let relsz = find_dyn_value(sz);
            if let (Some(rel), Some(relent), Some(relsz)) = (rel, relent, relsz) {
                Some((rel, relent, relsz))
            } else {
                None
            }
        };

        let flags = find_dyn_value(DT_FLAGS);
        let flags_1 = find_dyn_value(DT_FLAGS_1);
        if let Some(flags) = flags {
            if flags as i64 & DF_TEXTREL != 0 {
                error!("{}: relocations within text not supported", lib);
                return Err(DynlinkErrorKind::UnsupportedReloc {
                    library: lib.name.to_string(),
                    // TODO
                    reloc: "DF_TEXTREL".to_string(),
                }
                .into());
            }
        }
        debug!("{}: relocation flags: {:?} {:?}", lib, flags, flags_1);

        // Lookup all the tables
        let rels = find_dyn_rels(DT_REL, DT_RELENT, DT_RELSZ);
        let relas = find_dyn_rels(DT_RELA, DT_RELAENT, DT_RELASZ);
        let jmprels = find_dyn_rels(DT_JMPREL, DT_PLTREL, DT_PLTRELSZ);
        let _pltgot: Option<*const u8> = find_dyn_entry(DT_PLTGOT);

        let dynsyms = common
            .dynsyms
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynsyms".to_string(),
            })?;
        let dynsyms_str = common
            .dynsyms_strs
            .ok_or_else(|| DynlinkErrorKind::MissingSection {
                name: "dynsyms_strs".to_string(),
            })?;

        // Process relocations
        if let Some((rela, ent, sz)) = relas {
            self.process_rels(
                lib,
                rela,
                ent as usize,
                sz as usize,
                "RELA",
                &dynsyms_str,
                &dynsyms,
            )?;
        }

        if let Some((rel, ent, sz)) = rels {
            self.process_rels(
                lib,
                rel,
                ent as usize,
                sz as usize,
                "REL",
                &dynsyms_str,
                &dynsyms,
            )?;
        }

        // This one is a little special in that instead of an entry size, we are given a relocation
        // type.
        if let Some((rel, kind, sz)) = jmprels {
            let ent = match kind as i64 {
                DT_REL => 2,  // 2 usize long, according to ELF
                DT_RELA => 3, // one extra usize for the addend
                _ => {
                    error!("failed to relocate {}: unknown PLTREL type", lib);
                    return Err(DynlinkErrorKind::UnsupportedReloc {
                        library: lib.name.clone(),
                        reloc: "unknown PTREL type".to_string(),
                    }
                    .into());
                }
            } * size_of::<usize>();
            self.process_rels(lib, rel, ent, sz as usize, "JMPREL", &dynsyms_str, &dynsyms)?;
        }

        Ok(())
    }

    fn relocate_recursive(&mut self, root_id: LibraryId) -> Result<(), DynlinkError> {
        let lib = self.get_library(root_id)?;
        let libname = lib.name.to_string();
        match lib.reloc_state {
            crate::library::RelocState::Unrelocated => {}
            crate::library::RelocState::PartialRelocation => {
                error!("{}: tried to relocate a failed library", lib);
                return Err(DynlinkErrorKind::RelocationFail {
                    library: lib.name.to_string(),
                }
                .into());
            }
            crate::library::RelocState::Relocated => {
                trace!("{}: already relocated", lib);
                return Ok(());
            }
        }

        // We do this recursively instead of using a traversal, since we want to be able to prune
        // nodes that we know we no longer need to relocate. But since the reloc state gets
        // set at the end (so we can do this pruning), we'll need to track the visit states.
        // In the end, this is depth-first postorder.
        let deps = self
            .library_deps
            .neighbors_directed(root_id.0, petgraph::Direction::Outgoing)
            .collect::<Vec<_>>();

        let mut visit_state = HashSet::new();
        visit_state.insert(root_id.0);
        let rets = deps.into_iter().map(|dep_id| {
            if !visit_state.contains(&dep_id) {
                visit_state.insert(dep_id);
                self.relocate_recursive(LibraryId(dep_id))
            } else {
                Ok(())
            }
        });

        DynlinkError::collect(DynlinkErrorKind::DepsRelocFail { library: libname }, rets)?;

        // Okay, deps are ready, let's reloc the root.
        let lib = self.get_library_mut(root_id)?;
        lib.reloc_state = RelocState::PartialRelocation;

        let res = self.relocate_single(root_id);

        let lib = self.get_library_mut(root_id)?;
        if res.is_ok() {
            lib.reloc_state = RelocState::Relocated;
        } else {
            lib.reloc_state = RelocState::PartialRelocation;
        }
        res
    }

    /// Iterate through all libraries and process relocations for any libraries that haven't yet
    /// been relocated.
    pub fn relocate_all(&mut self, root_id: LibraryId) -> Result<(), DynlinkError> {
        let name = self.get_library(root_id)?.name.to_string();
        self.relocate_recursive(root_id).map_err(|e| {
            DynlinkError::new_collect(DynlinkErrorKind::RelocationFail { library: name }, vec![e])
        })
    }
}
