use elf::{endian::NativeEndian, string_table::StringTable, symbol::SymbolTable};
use tracing::error;
use twizzler_rt_abi::thread::TlsDesc;

use crate::{
    context::{relocate::EitherRel, Context},
    library::Library,
    symbol::LookupFlags,
    tls::{TlsRegion, TlsVariant},
    DynlinkError, DynlinkErrorKind,
};

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 8;

pub use elf::abi::{
    R_AARCH64_ABS64 as REL_SYMBOLIC, R_AARCH64_GLOB_DAT as REL_GOT,
    R_AARCH64_JUMP_SLOT as REL_JUMP, R_AARCH64_RELATIVE as REL_RELATIVE,
    R_AARCH64_TLSDESC as REL_TLSDESC, R_AARCH64_TLS_TPREL as REL_TPOFF,
};

#[repr(C)]
pub struct Tcb<T> {
    // implementation-defined TCB data
    pub dtv_len: usize,
    pub runtime_data: T,
    // aarch64 reserves the first 16 bytes of the TCB
    // TPIDR_EL0 is set to point here.
    pub dtv: *const usize,
    pub self_ptr: *const Tcb<T>,
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
    // A pointer to the TCB lies in the second word after TPIDR_EL0
    // we could calculate the address to that pointer and dereference
    // but simply calculating the address from the current position is faster.
    let mut val: usize;
    core::arch::asm!("mrs {}, tpidr_el0", out(reg) val);
    let offset = std::mem::size_of::<T>() + std::mem::size_of::<usize>();
    (val - offset) as *mut _
}

impl TlsRegion {
    /// Get a pointer to the thread control block for this TLS region.
    ///
    /// # Safety
    /// The TCB must actually contain runtime data of type T, and be initialized.
    pub unsafe fn get_thread_control_block<T>(&self) -> *mut Tcb<T> {
        let thread_pointer = self.thread_pointer.as_ptr();
        // the TCB exists above the thread pointer
        let byte_offset = std::mem::size_of::<T>() + std::mem::size_of::<usize>();
        let self_ptr = thread_pointer.sub(byte_offset);
        self_ptr.cast()
    }
}

impl Context {
    pub(crate) fn do_reloc(
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
                    .lookup_symbol(lib.id(), "_tlsdesc_static", flags)
                    .expect("failed to find tls descriptor symbol");
                desc.resolver = tls_resolver.reloc_value() as *const u8 as *mut core::ffi::c_void;

                // set the parameter to be used directly in the resolver function
                // calculate st_value + load_offset + addend
                if rel.sym() == 0 {
                    let tls_val = 0u64;
                    let module_offset = lib
                        .tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: lib.name.clone(),
                        })?
                        .offset();
                    desc.value = tls_val + module_offset as u64 + addend as u64;
                } else {
                    let sym_res = open_sym();
                    let tls_val = sym_res.as_ref().map(|sym| sym.raw_value()).unwrap_or(0);
                    let other_lib = sym_res?.lib;
                    let module_offset = other_lib
                        .tls_id
                        .as_ref()
                        .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                            library: other_lib.name.clone(),
                        })?
                        .offset();
                    desc.value = tls_val + module_offset as u64 + addend as u64;
                }
            }
            REL_RELATIVE => unsafe {
                // aarch64 calculates Delta(S) + A:
                // - relative adjustment to a place from the load address to its original link
                //   address
                // - Delta(S) is the difference between the load address and the staring address of
                //   the ELF file (first PT_LOAD segment)
                // - A is the addend

                let in_mem_addr = base;
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
            REL_GOT => unsafe {
                // calculate S + A
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend);
            },
            REL_JUMP => unsafe {
                // calculate S + A
                *target = open_sym()?.reloc_value().wrapping_add_signed(addend);
            },
            REL_TPOFF => unsafe {
                // calculate TPREL(S+A)
                // resolves to the offset from the current thread pointer (TP)
                // of the thread local variable located at offset A
                // from thread-local symbol S.
                let sym_res = open_sym()?;
                let other_lib = sym_res.lib;
                let tls_val = sym_res.raw_value();
                let module_offset = other_lib
                    .tls_id
                    .as_ref()
                    .ok_or_else(|| DynlinkErrorKind::NoTLSInfo {
                        library: other_lib.name.clone(),
                    })?
                    .offset();
                *target = tls_val.wrapping_add_signed(addend) + module_offset as u64;
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
