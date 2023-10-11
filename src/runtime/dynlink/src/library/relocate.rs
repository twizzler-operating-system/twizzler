use std::{mem::size_of, sync::Arc};

use elf::{
    abi::{
        DF_TEXTREL, DT_FLAGS, DT_FLAGS_1, DT_JMPREL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_REL,
        DT_RELA, DT_RELAENT, DT_RELASZ, DT_RELENT, DT_RELSZ, STB_WEAK,
    },
    endian::NativeEndian,
    parse::{ParseAt, ParsingIterator},
    relocation::{Rel, Rela},
    string_table::StringTable,
    symbol::SymbolTable,
};
use tracing::{debug, error, trace};

use crate::{
    context::ContextInner,
    library::RelocState,
    symbol::{LookupFlags, RelocatedSymbol},
    DynlinkError, ECollector,
};

use crate::arch::{
    REL_DTPMOD, REL_DTPOFF, REL_GOT, REL_PLT, REL_RELATIVE, REL_SYMBOLIC, REL_TPOFF,
};

use super::{Library, LibraryRef};

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

impl Library {
    pub(crate) fn laddr<T>(&self, val: u64) -> Option<*const T> {
        self.base_addr.map(|base| (base + val as usize) as *const T)
    }

    pub(crate) fn laddr_mut<T>(&self, val: u64) -> Option<*mut T> {
        self.base_addr.map(|base| (base + val as usize) as *mut T)
    }

    pub(crate) fn lookup_symbol(
        self: &Arc<Self>,
        name: &str,
    ) -> Result<RelocatedSymbol, DynlinkError> {
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
                        trace!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                }
            }
            return Err(DynlinkError::NotFound {
                name: name.to_string(),
            });
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
                        trace!("lookup symbol {} skipping weak binding in {}", name, self);
                    }
                }
            }
        }
        Err(DynlinkError::NotFound {
            name: name.to_string(),
        })
    }

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
        self: &LibraryRef,
        rel: EitherRel,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        ctx: &ContextInner,
    ) -> Result<(), DynlinkError> {
        let addend = rel.addend();
        let base = self.base_addr.unwrap() as u64;
        let target: *mut u64 = self.laddr_mut(rel.offset()).unwrap();
        // Lookup a symbol if the relocation's symbol index is non-zero.
        let symbol = if rel.sym() != 0 {
            let sym = syms.get(rel.sym() as usize)?;
            let flags = LookupFlags::empty();
            strings
                .get(sym.st_name as usize)
                .map(|name| (name, ctx.lookup_symbol(self, name, flags)))
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
                        self,
                        name,
                        sym.reloc_value(),
                        sym.lib
                    );
                    Ok(sym)
                } else {
                    error!("{}: needed symbol {} not found", self, name);
                    Err(DynlinkError::NotFound {
                        name: name.to_string(),
                    })
                }
            } else {
                error!("{}: invalid relocation, no symbol data", self);
                Err(DynlinkError::Unknown)
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
                    self.tls_id.as_ref().ok_or(DynlinkError::Unknown)?.tls_id()
                } else {
                    open_sym()?
                        .lib
                        .tls_id
                        .as_ref()
                        .ok_or(DynlinkError::Unknown)?
                        .tls_id()
                };
                unsafe { *target = id }
            }
            REL_DTPOFF => {
                let val = open_sym().map(|sym| sym.raw_value()).unwrap_or(0);
                unsafe { *target = val.wrapping_add_signed(addend) }
            }
            REL_TPOFF => {
                if let Some(tls) = self.tls_id {
                    let val = open_sym().map(|sym| sym.raw_value()).unwrap_or(0);
                    unsafe {
                        *target = val
                            .wrapping_sub(tls.offset() as u64)
                            .wrapping_add_signed(addend)
                    }
                } else {
                    error!("{}: TPOFF relocations require a PT_TLS segment", self);
                    Err(DynlinkError::Unknown)?
                }
            }
            _ => {
                error!("{}: unsupported relocation: {}", self, rel.r_type());
                Err(DynlinkError::Unknown)?
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn process_rels(
        self: &LibraryRef,
        start: *const u8,
        ent: usize,
        sz: usize,
        name: &str,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        ctx: &ContextInner,
    ) -> Result<(), DynlinkError> {
        debug!(
            "{}: processing {} relocations (num = {})",
            self,
            name,
            sz / ent
        );
        // Try to parse the table as REL or RELA, according to ent size. If get_parsing_iter succeeds for a given
        // relocation type, that's the correct one.
        if let Some(rels) = self.get_parsing_iter(start, ent, sz) {
            rels.map(|rel| self.do_reloc(EitherRel::Rel(rel), strings, syms, ctx))
                .ecollect::<Vec<_>>()?;
            Ok(())
        } else if let Some(relas) = self.get_parsing_iter(start, ent, sz) {
            relas
                .map(|rela| self.do_reloc(EitherRel::Rela(rela), strings, syms, ctx))
                .ecollect::<Vec<_>>()?;
            Ok(())
        } else {
            Err(DynlinkError::Unknown)
        }
    }

    pub(crate) fn relocate(self: &LibraryRef, ctx: &ContextInner) -> Result<(), DynlinkError> {
        // Atomically change state to relocating, using a CAS, to ensure a single thread gets the rights to relocate a library.
        if !self.try_set_reloc_state(RelocState::Unrelocated, RelocState::Relocating) {
            return Ok(());
        }
        // Recurse on dependencies first, in case there are any copy relocations.
        ctx.library_deps
            .neighbors_directed(self.idx.get().unwrap(), petgraph::Direction::Outgoing)
            .enumerate()
            .map(|(idx, depidx)| {
                if idx == 0 {
                    debug!("{}: relocating dependencies", self);
                }
                let dep = &ctx.library_deps[depidx];
                dep.relocate(ctx)
            })
            .ecollect()?;
        debug!("{}: relocating library", self);
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;
        let dynamic = common.dynamic.ok_or(DynlinkError::Unknown)?;

        // Helper to lookup a single entry for a relocated pointer in the dynamic table.
        let find_dyn_entry = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .and_then(|d| self.laddr(d.d_ptr()))
                .ok_or(DynlinkError::Unknown)
        };

        // Helper to lookup a single value in the dynamic table.
        let find_dyn_value = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| d.d_val())
                .ok_or(DynlinkError::Unknown)
        };

        // Many of the relocation tables are described in a similar way -- start, entry size, and table size (in bytes).
        let find_dyn_rels = |tag, ent, sz| {
            let rel = find_dyn_entry(tag).ok();
            let relent = find_dyn_value(ent).ok();
            let relsz = find_dyn_value(sz).ok();
            if let (Some(rel), Some(relent), Some(relsz)) = (rel, relent, relsz) {
                Some((rel, relent, relsz))
            } else {
                None
            }
        };

        let flags = find_dyn_value(DT_FLAGS).ok();
        let flags_1 = find_dyn_value(DT_FLAGS_1).ok();
        if let Some(flags) = flags {
            if flags as i64 & DF_TEXTREL != 0 {
                error!("{}: relocations within text not supported", self);
                return Err(DynlinkError::Unknown);
            }
        }
        debug!("{}: relocation flags: {:?} {:?}", self, flags, flags_1);

        // Lookup all the tables
        let rels = find_dyn_rels(DT_REL, DT_RELENT, DT_RELSZ);
        let relas = find_dyn_rels(DT_RELA, DT_RELAENT, DT_RELASZ);
        let jmprels = find_dyn_rels(DT_JMPREL, DT_PLTREL, DT_PLTRELSZ);
        let _pltgot: Option<*const u8> = find_dyn_entry(DT_PLTGOT).ok();

        let dynsyms = common.dynsyms.ok_or(DynlinkError::Unknown)?;
        let dynsyms_str = common.dynsyms_strs.ok_or(DynlinkError::Unknown)?;

        // Process relocations
        if let Some((rela, ent, sz)) = relas {
            self.process_rels(
                rela,
                ent as usize,
                sz as usize,
                "RELA",
                &dynsyms_str,
                &dynsyms,
                ctx,
            )?;
        }

        if let Some((rel, ent, sz)) = rels {
            self.process_rels(
                rel,
                ent as usize,
                sz as usize,
                "REL",
                &dynsyms_str,
                &dynsyms,
                ctx,
            )?;
        }

        // This one is a little special in that instead of an entry size, we are given a relocation type.
        if let Some((rel, kind, sz)) = jmprels {
            let ent = match kind as i64 {
                DT_REL => 2,  // 2 usize long, according to ELF
                DT_RELA => 3, // one extra usize for the addend
                _ => {
                    error!("failed to relocate {}: unknown PLTREL type", self);
                    return Err(DynlinkError::Unknown);
                }
            } * size_of::<usize>();
            self.process_rels(rel, ent, sz as usize, "JMPREL", &dynsyms_str, &dynsyms, ctx)?;
        }

        // We are the only ones who could get here, because of the CAS in try_set_state.
        self.set_reloc_state(RelocState::Relocated);
        Ok(())
    }
}
