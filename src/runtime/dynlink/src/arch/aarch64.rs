use crate::{
    context::{relocate::EitherRel, Context},
    library::Library,
    tls::{Tcb, TlsRegion},
    DynlinkError, DynlinkErrorKind,
};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::{
    R_AARCH64_ABS64 as REL_SYMBOLIC, R_AARCH64_COPY as REL_COPY, R_AARCH64_GLOB_DAT as REL_GOT,
    R_AARCH64_JUMP_SLOT as REL_PLT, R_AARCH64_RELATIVE as REL_RELATIVE,
    R_AARCH64_TLS_DTPMOD as REL_DTPMOD, R_AARCH64_TLS_DTPREL as REL_DTPOFF,
    R_AARCH64_TLS_TPREL as REL_TPOFF,
};
use elf::{endian::NativeEndian, string_table::StringTable, symbol::SymbolTable};

/// Get a pointer to the current thread control block, using the thread pointer.
///
/// # Safety
/// The TCB must actually contain runtime data of type T, and be initialized.
pub unsafe fn get_current_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    val as *mut _
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        todo!()
    }
}

impl Context {
    /// architechture specific symbol relocation implementation.
    ///
    /// More information can be found here:
    ///     https://github.com/ARM-software/abi-aa/releases/download/2024Q3/aaelf64.pdf
    pub fn do_reloc(
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
                .map(|name| {
                    (
                        name,
                        self.lookup_symbol(lib.id(), name, flags), // }
                    )
                })
                .ok()
        } else {
            None
        };

        // Helper for logging errors.
        let open_sym = || {
            if let Some((name, sym)) = symbol {
                if let Ok(sym) = sym {
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
            REL_RELATIVE => unsafe {
                // aarch64 calculates Delta(S) + A:
                // - relative adjustment to a place from the load address to its original link
                //   address
                // - Delta(S) is the difference between the load address and the staring address of
                //   the ELF file (first PT_LOAD segment)
                // - A is the addend

                let in_mem_addr = lib.base_addr() as u64;
                let elf = lib.get_elf().expect("failed to get elf");
                // the starting address in the elf file is the VA in the first PT_LOAD segment
                let load_hdrs = elf
                    .segments()
                    .ok_or_else(|| DynlinkErrorKind::MissingSection {
                        name: "segment info".to_string(),
                    })?
                    .iter()
                    .filter(|p| p.p_type == elf::abi::PT_LOAD);
                let mut elf_load_addr = 0;
                for hdr in load_hdrs {
                    // the first PT_LOAD segment is the one with the lowest address,
                    // so we find the minimum among all PT_LOAD segments
                    if elf_load_addr < hdr.p_vaddr {
                        elf_load_addr = hdr.p_vaddr;
                    }
                }

                *target = (in_mem_addr - elf_load_addr).wrapping_add_signed(addend);
            },
            REL_SYMBOLIC => unsafe {
                // calculate S + A
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend)
            },
            REL_PLT | REL_GOT => unsafe {
                // jump slot and glob dat operations
                // calculate S + A
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend);
            },
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
}
