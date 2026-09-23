use elf::{endian::NativeEndian, string_table::StringTable, symbol::SymbolTable};
use petgraph::graph::NodeIndex;
use tracing::error;
use twizzler_rt_abi::thread::{TlsDesc, TlsIndex};

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

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 8;

pub use elf::abi::{
    R_AARCH64_ABS64 as REL_SYMBOLIC, R_AARCH64_GLOB_DAT as REL_GOT,
    R_AARCH64_JUMP_SLOT as REL_JUMP, R_AARCH64_RELATIVE as REL_RELATIVE,
    R_AARCH64_TLSDESC as REL_TLSDESC, R_AARCH64_TLS_TPREL as REL_TPOFF, STB_WEAK,
};

/// Same layout as x86_64, and as mlibc's `Tcb` (tcb.hpp), which the runtime's `T` continues.
#[repr(C)]
pub struct Tcb<T> {
    pub self_ptr: *const Tcb<T>,
    pub dtv_len: usize,
    pub dtv: *const usize,
    pub runtime_data: T,
}

/// Distance from the control block to the thread pointer. TP..TP+16 is the reserved area the
/// AArch64 TLS ABI keeps before the first block, and mlibc locates its `Tcb` from TP as
/// `tp + 16 - sizeof(Tcb)` (144 bytes on Twizzler), so this is fixed whatever `T` is.
pub(crate) const TCB_BELOW_TP: usize = 128;

/// Bytes a TLS region reserves for the control block and the words at TP.
pub(crate) const fn tcb_reserve<T>() -> usize {
    assert!(std::mem::size_of::<Tcb<T>>() <= TCB_BELOW_TP + 16);
    TCB_BELOW_TP + 16
}

/// The control block a thread pointer value refers to.
pub fn tcb_from_thread_pointer<T>(tp: *mut u8) -> *mut Tcb<T> {
    tp.wrapping_sub(TCB_BELOW_TP).cast()
}

/// The thread pointer value for a control block.
pub fn thread_pointer_from_tcb<T>(tcb: *mut Tcb<T>) -> *mut u8 {
    tcb.cast::<u8>().wrapping_add(TCB_BELOW_TP)
}

/// Return the TLS variant defined by the arch-specific ABI.
pub const fn get_tls_variant() -> TlsVariant {
    TlsVariant::Variant1
}

/// Get a pointer to the current thread control block, using the thread pointer.
///
/// # Safety
/// The TCB must actually contain runtime data of type T, and be initialized.
pub unsafe fn get_current_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    tcb_from_thread_pointer(val as *mut u8)
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        tcb_from_thread_pointer(self.thread_pointer.as_ptr())
    }
}

impl Context {
    pub(crate) fn do_reloc(
        &self,
        lib: &Library,
        rel: EitherRel,
        strings: &StringTable,
        syms: &SymbolTable<NativeEndian>,
        deps: &[NodeIndex],
        reloc_cache: &mut RelocCache,
    ) -> Result<(), DynlinkError> {
        let base = lib.base_addr() as u64;
        let target: *mut u64 = lib.laddr_mut(rel.offset());
        let addend = rel.addend(target);
        let mut is_weak = false;
        // Lookup a symbol if the relocation's symbol index is non-zero.
        let symbol = if rel.sym() != 0 {
            let sym = syms.get(rel.sym() as usize)?;
            is_weak = sym.st_bind() == STB_WEAK;
            strings
                .get(sym.st_name as usize)
                .map(|name| {
                    // The per-compartment cache keeps multiply-defined names bound to one
                    // resolver across a compartment's libraries (see the x86_64 version).
                    let sym = match reloc_cache.find(name, lib.comp_id) {
                        Some(sym) => Ok(sym.clone()),
                        None => {
                            let sym =
                                self.lookup_symbol(lib.id(), name, LookupFlags::ALLOW_WEAK, deps);
                            if let Ok(ref sym) = sym {
                                reloc_cache.insert(name, lib.comp_id, unsafe {
                                    std::mem::transmute(sym.clone())
                                });
                            }
                            sym
                        }
                    };
                    (name, sym)
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
            REL_TLSDESC => {
                // calculate: TLSDESC(S+A)
                //
                // TLS descriptors are a fast way of resolving a symbol.
                // TLS descriptors are allocated two pointer sized GOT entries,
                // one being a function pointer to the resolver function,
                // and another being an argument to be used by that resolver,
                // typically the offset to that variable from the thread
                // pointer register. Resolver functions are defined in libc
                // so that they can be referenced by any program.

                // get a pointer to the TLS descriptor
                let desc_ptr = target.cast::<TlsDesc>();
                let desc = unsafe { &mut *desc_ptr };

                // set the TLS descriptor resolver function
                let flags = LookupFlags::empty();
                let tls_resolver = self
                    .lookup_symbol(lib.id(), "_tlsdesc_static", flags, deps)
                    .expect("failed to find tls descriptor symbol");
                desc.resolver = tls_resolver.reloc_value() as *const u8 as *mut core::ffi::c_void;

                // set the parameter to be used directly in the resolver function
                // calculate st_value + load_offset + addend
                let (tls_val, target_lib) = if rel.sym() == 0 {
                    (0u64, lib)
                } else {
                    let sym_res = open_sym();
                    let tls_val = sym_res.as_ref().map(|sym| sym.raw_value()).unwrap_or(0);
                    (tls_val, sym_res?.lib)
                };
                let tls_id = target_lib.tls_id.as_ref().ok_or_else(|| {
                    DynlinkErrorKind::NoTLSInfo {
                        library: target_lib.name.as_str().into(),
                    }
                })?;
                // `_tlsdesc_static` resolves to a TP-relative constant, which is only valid if
                // the target module is in every thread's static region. A runtime-loaded module
                // is absent from already-running threads' regions, so it goes through
                // `_tlsdesc_dynamic` with a `tls_index` argument, like x86's DTPMOD/DTPOFF pair.
                // The index outlives the library: leaked, as unloading is not supported.
                if target_lib.runtime_load {
                    let tls_resolver = self
                        .lookup_symbol(lib.id(), "_tlsdesc_dynamic", flags, deps)
                        .expect("failed to find dynamic tls descriptor symbol");
                    desc.resolver =
                        tls_resolver.reloc_value() as *const u8 as *mut core::ffi::c_void;
                    let index = Box::leak(Box::new(TlsIndex {
                        mod_id: tls_id.tls_id() as usize,
                        offset: (tls_val + addend as u64) as usize,
                    }));
                    desc.value = index as *const TlsIndex as u64;
                } else {
                    desc.value = tls_val + tls_id.offset() as u64 + addend as u64;
                }
            }
            // Delta(S) + A, and Delta(S) is the load base: images link at 0.
            REL_RELATIVE => unsafe { *target = base.wrapping_add_signed(addend) },
            REL_SYMBOLIC => unsafe {
                // calculate S + A
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend)
            },
            // S, never S + A: with REL-form PLT entries (libc.so, linked by the clang driver) the
            // slot's "addend" is the lazy-binding stub address.
            REL_GOT | REL_JUMP => unsafe { *target = open_sym()?.reloc_value() },
            REL_TPOFF => unsafe {
                // calculate TPREL(S+A)
                // resolves to the offset from the current thread pointer (TP)
                // of the thread local variable located at offset A
                // from thread-local symbol S.
                // sym 0: an offset into this library's own TLS block (local IE TLS), as on x86.
                let (tls_val, other_lib) = if rel.sym() == 0 {
                    (0, lib)
                } else {
                    let sym_res = open_sym()?;
                    (sym_res.raw_value(), sym_res.lib)
                };
                // Same constraint as TLSDESC above: TP-relative offsets into a runtime-loaded
                // module are invalid for already-running threads.
                if other_lib.runtime_load {
                    error!(
                        "{}: initial-exec TLS relocation targets runtime-loaded module {}",
                        lib, other_lib
                    );
                    return Err(DynlinkErrorKind::UnsupportedReloc {
                        library: lib.name.as_str().into(),
                        reloc: "TPOFF against runtime-loaded module".into(),
                    }
                    .into());
                }
                let module_offset = other_lib
                    .tls_id
                    .as_ref()
                    .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                        library: other_lib.name.as_str().into(),
                    })?
                    .offset();
                *target = tls_val.wrapping_add_signed(addend) + module_offset as u64;
            },
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

        Ok(())
    }
}
