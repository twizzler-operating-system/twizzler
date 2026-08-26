use elf::{endian::NativeEndian, string_table::StringTable, symbol::SymbolTable};
use petgraph::graph::NodeIndex;
use tracing::error;

use crate::{
    context::{
        relocate::{EitherRel, RelocCache},
        Context,
    },
    library::Library,
    symbol::LookupFlags,
    tls::{TlsRegion, TlsVariant},
    DynlinkError, DynlinkErrorKind,
};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub use elf::abi::{
    R_X86_64_64 as REL_SYMBOLIC, R_X86_64_DTPMOD64 as REL_DTPMOD, R_X86_64_DTPOFF64 as REL_DTPOFF,
    R_X86_64_GLOB_DAT as REL_GOT, R_X86_64_JUMP_SLOT as REL_PLT, R_X86_64_RELATIVE as REL_RELATIVE,
    R_X86_64_TPOFF64 as REL_TPOFF, STB_WEAK,
};

#[repr(C)]
pub struct Tcb<T> {
    pub self_ptr: *const Tcb<T>,
    pub dtv_len: usize,
    pub dtv: *const usize,
    pub runtime_data: T,
}

/// Return the TLS variant defined by the arch-specific ABI.
pub fn get_tls_variant() -> TlsVariant {
    TlsVariant::Variant2
}

/// Get a pointer to the current thread control block, using the thread pointer.
///
/// # Safety
/// The TCB must actually contain runtime data of type T, and be initialized.
pub unsafe fn get_current_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mov {}, fs:0", out(reg) val);
    val as *mut _
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        self.get_thread_pointer_value() as *mut _
    }
}

impl Context {
    pub(crate) fn do_reloc(
        &self,
        lib: &Library,
        rel: EitherRel,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        deps_list: &[NodeIndex],
        reloc_cache: &mut RelocCache<'_>,
    ) -> Result<(), DynlinkError> {
        let base = lib.base_addr() as u64;
        let target: *mut u64 = lib.laddr_mut(rel.offset());
        let addend = rel.addend(target);
        let mut is_weak = false;
        // Lookup a symbol if the relocation's symbol index is non-zero.
        let symbol = if rel.sym() != 0 {
            let sym = syms.get(rel.sym() as usize)?;
            let flags = if sym.st_bind() == STB_WEAK {
                is_weak = true;
                LookupFlags::ALLOW_WEAK
            } else {
                LookupFlags::ALLOW_WEAK
            };
            // No per-relocation timing: this runs for every symbolic relocation.
            let r = strings
                .get(sym.st_name as usize)
                .map(|name| {
    // Replay first: a memo hit skips the blooms and the whole search. It must still
                    // populate the per-compartment cache: multiply-defined names (`malloc` lives
                    // in both libc and twz-rt's shims) are kept order-uniform across a
                    // compartment's libraries *by* that cache -- the first resolver wins and
                    // everyone later reuses it. Skipping the insert let a later library re-search
                    // by its own deps order and bind the twz-rt stub instead of libc, which killed
                    // pager-srv's first C allocation (verifymemo round 1).
                    let sym = if let Some(msym) = reloc_cache.memo_probe(self, name, deps_list) {
                        reloc_cache.insert(name, lib.comp_id, unsafe {
                            std::mem::transmute(msym.clone())
                        });
                        if crate::context::relocate::RELOC_MEMO_VERIFY {
                            let live = self.lookup_symbol(lib.id(), name, flags, deps_list);
                            let agrees = live.as_ref().is_ok_and(|l| {
                                core::ptr::eq(
                                    l.lib as *const Library,
                                    msym.lib as *const Library,
                                ) && l.raw_value() == msym.raw_value()
                            });
                            if !agrees {
                                reloc_cache.memo_bad += 1;
                            }
                        }
                        Ok(msym)
                    } else {
                        match reloc_cache.find(name, lib.comp_id) {
                            Some(sym) => {
                                tracing::trace!("found {} in cache", name);
                                Ok(sym.clone())
                            }
                            None => {
                                // Only misses are timed; see RelocCache::resolve_time.
                                let _t = std::time::Instant::now();
                                let sym = self.lookup_symbol(lib.id(), name, flags, deps_list);
                                reloc_cache.resolve_time += _t.elapsed();
                                if let Ok(ref sym) = sym {
                                    reloc_cache.memo_record(lib, name, sym, deps_list);
                                    reloc_cache.insert(name, lib.comp_id, unsafe {
                                        std::mem::transmute(sym.clone())
                                    });
                                }
                                sym
                            }
                        }
                    };

                    (name, sym)
                })
                .ok();
            r
        } else {
            None
        };
        let sn = symbol.as_ref().map(|s| s.0).unwrap_or_default();

        // Helper for logging errors.
        let open_sym = || {
            if let Some((name, sym)) = symbol {
                if let Ok(sym) = sym {
                    Result::<_, DynlinkError>::Ok(sym)
                } else if is_weak {
                    Result::<_, DynlinkError>::Ok(crate::symbol::RelocatedSymbol::new_zero(lib))
                } else {
                    error!("{}: needed symbol {} not found", lib, name);
                    Err(DynlinkErrorKind::SymbolLookupFail {
                        symname: name.into(),
                        sourcelib: lib.name.as_str().into(),
                    }
                    .into())
                }
            } else {
                error!("{}: invalid relocation, no symbol data", lib);
                Err(DynlinkErrorKind::MissingSection {
                    name: "symbol data".into(),
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
            REL_PLT | REL_GOT => unsafe {
                let x = open_sym()?.reloc_value();
                *target = x;
            },
            REL_DTPMOD => {
                // See the TLS module for understanding where the TLS ID is coming from.
                let id = if rel.sym() == 0 {
                    lib.tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: lib.name.as_str().into(),
                        })?
                        .tls_id()
                } else {
                    let other_lib = open_sym()?.lib;
                    other_lib
                        .tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: other_lib.name.as_str().into(),
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
                // sym 0: an offset into this library's own TLS block (local IE TLS), same
                // convention REL_DTPMOD handles above.
                let (raw, tls_id) = if rel.sym() == 0 {
                    (0, lib.tls_id)
                } else {
                    let sym = open_sym()?;
                    (sym.raw_value(), sym.lib.tls_id)
                };
                if let Some(tls) = tls_id {
                    unsafe {
                        *target = raw
                            .wrapping_sub(tls.offset() as u64)
                            .wrapping_add_signed(addend)
                    }
                } else {
                    error!(
                        "{}: TPOFF relocations require a PT_TLS segment (sym {})",
                        lib, sn
                    );
                    Err(DynlinkErrorKind::NoTLSInfo {
                        library: lib.name.as_str().into(),
                    })?
                }
            }
            _ => {
                error!("{}: unsupported relocation: {}", lib, rel.r_type());
                Result::<_, DynlinkError>::Err(
                    DynlinkErrorKind::UnsupportedReloc {
                        library: lib.name.as_str().into(),
                        reloc: rel.r_type().to_string().into(),
                    }
                    .into(),
                )?
            }
        }
        tracing::trace!("set reloc {} to {:x}", rel.r_type(), unsafe { *target });

        Ok(())
    }
}
