//! Implements the core runtime functions.

use core::{alloc::GlobalAlloc, ptr};

use twizzler_runtime_api::{AuxEntry, BasicAux, BasicReturn, CoreRuntime};

use crate::object::ObjID;

use super::{
    alloc::MinimalAllocator,
    phdrs::{process_phdrs, Phdr},
    tls::init_tls,
    MinimalRuntime,
};

// Just keep a single, simple global allocator.
static GLOBAL_ALLOCATOR: MinimalAllocator = MinimalAllocator::new();

extern "C" {
    // These are defined in the linker script.
    static __preinit_array_start: extern "C" fn();
    static __preinit_array_end: extern "C" fn();
    static __init_array_start: extern "C" fn();
    static __init_array_end: extern "C" fn();

    // Defined via crti and crtn.
    fn _init();
}

impl CoreRuntime for MinimalRuntime {
    fn default_allocator(&self) -> &'static dyn GlobalAlloc {
        &GLOBAL_ALLOCATOR
    }

    fn exit(&self, code: i32) -> ! {
        crate::syscall::sys_thread_exit(code as u64);
    }

    fn abort(&self) -> ! {
        core::intrinsics::abort();
    }

    /// Called from _start to initialize the runtime and pass control to the Rust stdlib.
    fn runtime_entry(
        &self,
        mut aux_array: *const AuxEntry,
        std_entry: unsafe extern "C" fn(BasicAux) -> BasicReturn,
    ) -> ! {
        // If aux doesn't give us an environment, just use this default.
        let null_env: [*const i8; 4] = [
            b"RUST_BACKTRACE=1\0".as_ptr() as *const i8,
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
                        super::debug::set_execid(ObjID::new(id));
                    }
                    AuxEntry::Arguments(num, ptr) => {
                        arg_count = num;
                        arg_ptr = ptr as *const *const i8
                    }
                    AuxEntry::Environment(ptr) => {
                        env_ptr = ptr as *const *const i8;
                    }
                    _ => {
                        crate::print_err("unknown aux type");
                    }
                }
                aux_array = aux_array.offset(1);
            }
        }

        let tls = init_tls();
        if let Some(tls) = tls {
            crate::syscall::sys_thread_settls(tls);
        } else {
            crate::print_err("failed to initialize TLS\n");
        }
        crate::print_err("setting upcall");
        crate::syscall::sys_thread_set_upcall(crate::arch::upcall::upcall_entry);

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

        let ret = unsafe {
            std_entry(BasicAux {
                argc: arg_count,
                args: arg_ptr,
                env: env_ptr,
            })
        };
        super::__twz_get_runtime().exit(ret.code)
    }
}
