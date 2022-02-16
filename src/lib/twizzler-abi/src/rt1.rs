//! Runtime functions for Twizzler userspace. This part of the code is a bit arcane and somewhat
//! tricky, so buckle up.
//!
//! We need to start executing in a _reasonable_ environment before we call into the Rust runtime
//! init. Rust actually expects a fair bit of us right off the bat --- thread-local storage (TLS), env
//! vars, args, etc. So our goal will be to set up an environment where we can serve the right
//! underlying runtime to Rust.
//!
//! Execution will start at the _start symbol, provided in arch::_start. This will almost
//! immediately call [twz_runtime_start]. From there, we:
//!   0. Initialize global context.
//!   1. Process the aux array.
//!   2. Find the TLS template region and store that info.
//!   3. Create a TLS region for ourselves, the main thread.
//!   4. Set the TLS region via the kernel.
//!   5. Run the pre-init array, _init(), and the init_array.
//!   6. Call std_runtime_start, which jumps into the Rust standard lib.
//!   7. Exit our thread should we return here.
//!
//! And all of that has to happen without a panic runtime, so any errors we encounter we need to
//! abort(). This is what we get for not linking to libc.
//!
//! This does not encompass all of the runtime pieces. We also have:
//!   1. crti and crtn, which we provide, see toolchain/src/crti.rs etc.
//!   2. crtbegin and friends. These are provided by LLVM's crtstuff and distributed with the
//!      toolchain. They have interesting linking requirements, see below.
//!   3. libunwind. Not strictly required as part of the runtime, but we build with panic_unwind for
//!      userspace by default, so I'm including it. This also comes from llvm.
//!
//! For information about linking order and how the linking actually happens, take a look at
//! toolchain/src/rust/compiler/rustc_target/spec/{twizzler_base.rs, x86_64-unknown-twizzler.rs, x86_64-unknown-twizzler-linker-script.ld}.

use crate::object::ObjID;

extern "C" {
    // Defined in the rust stdlib.
    fn std_runtime_start(argc: usize, args: *const *const i8, env: *const *const i8) -> i32;

    // These are defined in the linker script.
    static __preinit_array_start: extern "C" fn();
    static __preinit_array_end: extern "C" fn();
    static __init_array_start: extern "C" fn();
    static __init_array_end: extern "C" fn();

    // Defined via crti and crtn.
    fn _init();

}

#[repr(C)]
struct Phdr {
    ty: u32,
    flags: u32,
    off: u64,
    vaddr: u64,
    paddr: u64,
    filesz: u64,
    memsz: u64,
    align: u64,
}

struct TlsInfo {
    template_start: *const u8,
    memsz: usize,
    filsz: usize,
    align: usize,
}

static mut TLS_INFO: Option<TlsInfo> = None;
static mut EXEC_ID: ObjID = ObjID::new(0);
static mut PHDR_INFO: Option<&'static [Phdr]> = None;

// TODO: this is a hack
pub(crate) unsafe fn get_exec_id() -> Option<ObjID> {
    let id = EXEC_ID;
    if id == 0.into() {
        None
    } else {
        Some(id)
    }
}

// TODO: this is a hack
pub(crate) fn get_load_seg(nr: usize) -> Option<(usize, usize)> {
    if let Some(phdrs) = unsafe { PHDR_INFO } {
        if nr < phdrs.len() {
            Some((phdrs[nr].vaddr as usize, phdrs[nr].memsz as usize))
        } else {
            None
        }
    } else {
        None
    }
}

const MIN_TLS_ALIGN: usize = 16;
use core::alloc::Layout;
fn init_tls() -> Option<u64> {
    new_thread_tls().map(|(s, _, _, _)| s as u64)
}

//let (tls_set, tls_base, tls_len, tls_align) = crate::rt1::new_thread_tls();
pub(crate) fn new_thread_tls() -> Option<(usize, *mut u8, usize, usize)> {
    unsafe {
        TLS_INFO.as_ref().map(|info| {
            let mut tls_size = info.memsz;
            tls_size += (((!tls_size) + 1) - (info.template_start as usize)) & (info.align - 1);
            let offset = tls_size;
            let tls_align = core::cmp::max(info.align, MIN_TLS_ALIGN);
            let full_tls_size =
                core::mem::size_of::<*const u8>() + tls_size + tls_align + MIN_TLS_ALIGN - 1
                    & ((!MIN_TLS_ALIGN) + 1);

            let layout = crate::internal_unwrap(
                Layout::from_size_align(full_tls_size, MIN_TLS_ALIGN).ok(),
                "failed to unwrap TLS layout",
            );
            let tls = crate::alloc::global_alloc(layout);
            if tls.is_null() {
                crate::print_err("failed to allocate TLS");
                crate::abort();
            }
            ptr::write_bytes(tls, 0x00, layout.size());
            let mem = tls.add(tls_size).sub((tls as usize) & (tls_align - 1));
            core::ptr::copy_nonoverlapping(info.template_start, mem.sub(offset), info.filsz);
            *(mem as *mut u64) = mem as u64;
            (mem as usize, tls, layout.size(), layout.align())
        })
    }
}

#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
fn process_phdrs(phdrs: &'static [Phdr]) {
    for ph in phdrs {
        if ph.ty == 7 {
            unsafe {
                TLS_INFO = Some(TlsInfo {
                    template_start: ph.vaddr as *const u8,
                    memsz: ph.memsz as usize,
                    filsz: ph.filesz as usize,
                    align: ph.align as usize,
                })
            }
        }
    }
    unsafe {
        PHDR_INFO = Some(phdrs);
    }
}

use crate::aux::AuxEntry;
use core::ptr;
#[allow(named_asm_labels)]
#[allow(unreachable_code)]
#[allow(unused_variables)]
#[allow(unused_mut)]
/// Called from _start to initialize the runtime and pass control to the Rust stdlib.
pub extern "C" fn twz_runtime_start(mut aux_array: *const AuxEntry) -> ! {
    crate::slot::runtime_init();
    let null_env: [*const i8; 4] = [
        b"RUST_BACKTRACE=full\0".as_ptr() as *const i8,
        ptr::null(),
        ptr::null(),
        ptr::null(),
    ];
    let mut arg_ptr = ptr::null();
    let mut arg_count = 0;
    let mut env_ptr = (&null_env).as_ptr();
    unsafe {
        while !aux_array.is_null() && *aux_array != AuxEntry::Null {
            match *aux_array {
                AuxEntry::ProgramHeaders(paddr, pnum) => {
                    process_phdrs(core::slice::from_raw_parts(paddr as *const Phdr, pnum))
                }
                AuxEntry::ExecId(id) => {
                    EXEC_ID = id;
                }
                AuxEntry::Arguments(num, ptr) => {
                    arg_count = num;
                    arg_ptr = ptr as *const *const i8
                }
                AuxEntry::Environment(ptr) => {
                    env_ptr = ptr as *const *const i8;
                }
                _ => {}
            }
            aux_array = aux_array.offset(1);
        }
    }
    let tls = init_tls();
    if let Some(tls) = tls {
        crate::syscall::sys_thread_settls(tls);
    }

    unsafe {
        // Run preinit array
        {
            let mut f = &__preinit_array_start as *const _;
            #[allow(clippy::op_ref)]
            while f < &__preinit_array_end {
                (*f)();
                f = f.offset(1);
            }
        }

        // Call init section
        _init();

        // Run init array
        {
            let mut f = &__init_array_start as *const _;
            #[allow(clippy::op_ref)]
            while f < &__init_array_end {
                (*f)();
                f = f.offset(1);
            }
        }
    }

    /* it's unsafe because it's an extern C function. */
    /* TODO: pass env and args */
    // let code = unsafe { std_runtime_start(arg_count, arg_ptr, &null_env as *const *const i8) };
    let code = unsafe { std_runtime_start(arg_count, arg_ptr, env_ptr) };
    //TODO: exit val
    crate::syscall::sys_thread_exit(code as u64, ptr::null_mut())
}
