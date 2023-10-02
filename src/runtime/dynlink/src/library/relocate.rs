use std::mem::size_of;

use elf::{
    abi::{
        DF_TEXTREL, DT_FLAGS, DT_FLAGS_1, DT_JMPREL, DT_PLTGOT, DT_PLTREL, DT_PLTRELSZ, DT_REL,
        DT_RELA, DT_RELACOUNT, DT_RELAENT, DT_RELASZ, DT_RELCOUNT, DT_RELENT, DT_RELSZ,
        R_X86_64_64, R_X86_64_DTPMOD64, R_X86_64_DTPOFF64, R_X86_64_GLOB_DAT, R_X86_64_JUMP_SLOT,
        R_X86_64_RELATIVE,
    },
    endian::NativeEndian,
    parse::{ParseAt, ParsingIterator},
    relocation::{Rel, Rela},
    string_table::StringTable,
    symbol::SymbolTable,
};
use tracing::{debug, error, trace, warn};
use twizzler_object::Object;

use crate::{compartment::Compartment, symbol::RelocatedSymbol, DynlinkError, ECollector};

use super::Library;

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
            EitherRel::Rel(r) => 0,
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
        self.get_base_addr()
            .map(|base| (base + val as usize) as *const T)
    }

    pub(crate) fn laddr_mut<T>(&self, val: u64) -> Option<*mut T> {
        self.get_base_addr()
            .map(|base| (base + val as usize) as *mut T)
    }

    pub(crate) fn get_base_addr(&self) -> Option<usize> {
        todo!()
    }

    pub(crate) fn lookup_symbol(&self, name: &str) -> Result<RelocatedSymbol, DynlinkError> {
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;

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
                return Ok(RelocatedSymbol::new(sym, self.get_base_addr().unwrap()));
            }
        }

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
                return Ok(RelocatedSymbol::new(sym, self.get_base_addr().unwrap()));
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
        &self,
        rel: EitherRel,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        comp: &Compartment,
    ) -> Result<(), DynlinkError> {
        let addend = rel.addend();
        let base = self.get_base_addr().unwrap() as u64;
        let target: *mut u64 = self.laddr_mut(rel.offset()).unwrap();
        let symbol = if rel.sym() != 0 {
            let sym = syms.get(rel.sym() as usize)?;
            strings
                .get(sym.st_name as usize)
                .map(|name| (name, comp.lookup_symbol(name)))
                .ok()
        } else {
            None
        };

        let open_sym = || {
            if let Some((name, sym)) = symbol {
                if let Ok(sym) = sym {
                    trace!("{}: found symbol {} at {:x}", self, name, sym.value());
                    Ok(sym.value())
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

        match rel.r_type() {
            R_X86_64_RELATIVE => unsafe { *target = base.wrapping_add_signed(addend) },
            R_X86_64_64 => unsafe { *target = open_sym()?.wrapping_add_signed(addend) },
            R_X86_64_JUMP_SLOT | R_X86_64_GLOB_DAT => unsafe { *target = open_sym()? },
            R_X86_64_DTPMOD64 => {
                warn!("not yet implemented: DTPMOD")
            }
            R_X86_64_DTPOFF64 => {
                warn!("not yet implemented: DTPOFF")
            }
            _ => {
                error!("{}: unsupported relocation: {}", self, rel.r_type());
                Err(DynlinkError::Unknown)?
            }
        }

        Ok(())
    }

    fn process_rels(
        &self,
        start: *const u8,
        ent: usize,
        sz: usize,
        name: &str,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        comp: &Compartment,
    ) -> Result<(), DynlinkError> {
        debug!(
            "{}: processing {} relocations (num = {})",
            self,
            name,
            sz / ent
        );
        if let Some(rels) = self.get_parsing_iter(start, ent, sz) {
            rels.map(|rel| self.do_reloc(EitherRel::Rel(rel), strings, syms, comp))
                .ecollect::<Vec<_>>()?;
            Ok(())
        } else if let Some(relas) = self.get_parsing_iter(start, ent, sz) {
            relas
                .map(|rela| self.do_reloc(EitherRel::Rela(rela), strings, syms, comp))
                .ecollect::<Vec<_>>()?;
            Ok(())
        } else {
            Err(DynlinkError::Unknown)
        }
    }

    #[allow(unused_variables)]
    pub(crate) fn relocate(&self, comp: &Compartment) -> Result<(), DynlinkError> {
        debug!("relocating library {}", self);
        let elf = self.get_elf()?;
        let common = elf.find_common_data()?;
        let dynamic = common.dynamic.ok_or(DynlinkError::Unknown)?;

        let find_dyn_entry = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| self.laddr(d.d_ptr()))
                .flatten()
                .ok_or(DynlinkError::Unknown)
        };

        let find_dyn_value = |tag| {
            dynamic
                .iter()
                .find(|d| d.d_tag == tag)
                .map(|d| d.d_val())
                .ok_or(DynlinkError::Unknown)
        };

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

        let rels = find_dyn_rels(DT_REL, DT_RELENT, DT_RELSZ);
        let relas = find_dyn_rels(DT_RELA, DT_RELAENT, DT_RELASZ);
        let jmprels = find_dyn_rels(DT_JMPREL, DT_PLTREL, DT_PLTRELSZ);
        let pltgot: Option<*const u8> = find_dyn_entry(DT_PLTGOT).ok();

        let dynsyms = common.dynsyms.ok_or(DynlinkError::Unknown)?;
        let dynsyms_str = common.dynsyms_strs.ok_or(DynlinkError::Unknown)?;

        if let Some((rela, ent, sz)) = relas {
            self.process_rels(
                rela,
                ent as usize,
                sz as usize,
                "RELA",
                &dynsyms_str,
                &dynsyms,
                comp,
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
                comp,
            )?;
        }

        if let Some((rel, kind, sz)) = jmprels {
            let ent = match kind as i64 {
                DT_REL => 2,
                DT_RELA => 3,
                _ => {
                    error!("failed to relocate {}: unknown PLTREL type", self);
                    return Err(DynlinkError::Unknown);
                }
            } * size_of::<usize>();
            self.process_rels(
                rel,
                ent,
                sz as usize,
                "JMPREL",
                &dynsyms_str,
                &dynsyms,
                comp,
            )?;
        }

        Ok(())
    }
}
