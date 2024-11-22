use tracing::{error, trace};

use crate::{
    context::{relocate::EitherRel, Context},
    library::Library,
    symbol::LookupFlags,
    tls::{TlsRegion, TlsVariant},
    DynlinkError, DynlinkErrorKind,
};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

use elf::{
    abi::{
        R_X86_64_64 as REL_SYMBOLIC, R_X86_64_COPY as REL_COPY, R_X86_64_DTPMOD64 as REL_DTPMOD,
        R_X86_64_DTPOFF64 as REL_DTPOFF, R_X86_64_GLOB_DAT as REL_GOT,
        R_X86_64_JUMP_SLOT as REL_PLT, R_X86_64_RELATIVE as REL_RELATIVE,
        R_X86_64_TPOFF64 as REL_TPOFF,
    },
    endian::NativeEndian,
    string_table::StringTable,
    symbol::SymbolTable,
};

#[repr(C)]
pub struct Tcb<T> {
    pub self_ptr: *const Tcb<T>,
    pub dtv: *const usize,
    pub dtv_len: usize,
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
}
