//! Implements the core runtime functions.

use core::{alloc::GlobalAlloc, ptr};

use twizzler_rt_abi::core::{BasicAux, BasicReturn, RuntimeInfo, RUNTIME_INIT_MIN};
use twizzler_rt_abi::info::SystemInfo;
use twizzler_rt_abi::time::Monotonicity;

use super::{
    alloc::MinimalAllocator,
    phdrs::{process_phdrs, Phdr},
    tls::init_tls,
    MinimalRuntime,
};
use crate::{
    object::ObjID,
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
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

impl MinimalRuntime {
    pub fn default_allocator(&self) -> &'static dyn GlobalAlloc {
        &GLOBAL_ALLOCATOR
    }

    pub fn exit(&self, code: i32) -> ! {
        crate::syscall::sys_thread_exit(code as u64);
    }

    pub fn abort(&self) -> ! {
        core::intrinsics::abort();
    }

    pub fn pre_main_hook(&self) -> Option<i32> {
        None
    }

    pub fn post_main_hook(&self) {}

    /// Called from _start to initialize the runtime and pass control to the Rust stdlib.
    pub fn runtime_entry(
        &self,
        rt_info: *const RuntimeInfo,
        std_entry: unsafe extern "C-unwind" fn(BasicAux) -> BasicReturn,
    ) -> ! {
        let mut null_env: [*mut i8; 4] = [
            b"RUST_BACKTRACE=1\0".as_ptr() as *mut i8,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        ];
        let mut arg_ptr = ptr::null_mut();
        let mut arg_count = 0;
        let mut env_ptr = (&mut null_env).as_mut_ptr();

        unsafe {
            let rt_info = rt_info.as_ref().unwrap();
            if rt_info.kind != RUNTIME_INIT_MIN {
                crate::print_err("minimal runtime cannot initialize non-minimal runtime");
                self.abort();
            } 
            let min_init_info = &*rt_info.init_info.min;
            process_phdrs(core::slice::from_raw_parts(min_init_info.phdrs as *const Phdr, min_init_info.nr_phdrs));
            if !min_init_info.envp.is_null() {
                env_ptr = min_init_info.envp;
            }
            if !min_init_info.args.is_null() {
                arg_ptr = min_init_info.args;
                arg_count = min_init_info.argc;
            }
        }

        let tls = init_tls();
        if let Some(tls) = tls {
            crate::syscall::sys_thread_settls(tls);
        } else {
            crate::print_err("failed to initialize TLS\n");
        }
        let upcall_target = UpcallTarget::new(
            Some(crate::arch::upcall::upcall_entry),
            Some(crate::arch::upcall::upcall_entry),
            0,
            0,
            0,
            0.into(),
            [UpcallOptions {
                flags: UpcallFlags::empty(),
                mode: UpcallMode::CallSelf,
            }; UpcallInfo::NR_UPCALLS],
        );
        crate::syscall::sys_thread_set_upcall(upcall_target);

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
        self.exit(ret.code)
    }

    pub fn sysinfo(&self) -> SystemInfo {
        let info = crate::syscall::sys_info();
        SystemInfo {
            clock_monotonicity: Monotonicity::Weak.into(),
            available_parallelism: info.cpu_count().into(),
            page_size: info.page_size(),
        }
    }

    pub fn get_random(&self, buf: &mut [u8]) -> usize {
        // TODO: Once the Randomness PR is in, fix this.
        buf.len()
    }
}

pub mod rt0 {
    //! rt0 defines a collection of functions that the basic Rust ABI expects to be defined by some part
    //! of the C runtime:
    //!
    //!   - __tls_get_addr for handling non-local TLS regions.
    //!   - _start, the entry point of an executable (per-arch, as this is assembly code).

    #[cfg(target_arch = "aarch64")]
    #[no_mangle]
    #[naked]
    pub unsafe extern "C" fn _start() {
        core::arch::naked_asm!(
            "b {entry}",
            entry = sym entry,
        );
    }

    #[cfg(target_arch = "x86_64")]
    #[no_mangle]
    #[naked]
    pub unsafe extern "C" fn _start() {
        // Align the stack and jump to rust code. If we come back, trigger an exception.
        core::arch::naked_asm!(
            "and rsp, 0xfffffffffffffff0",
            "call {entry}",
            "ud2",
            entry = sym entry,
        );
    }

    #[used]
    // Ensure the compiler doesn't optimize us away!
    static ENTRY: unsafe extern "C" fn() = _start;

    use twizzler_rt_abi::core::{BasicAux, BasicReturn, RuntimeInfo};
    
    // The C-based entry point coming from arch-specific assembly _start function.
    unsafe extern "C" fn entry(arg: usize) -> ! {
        // Just trampoline to rust-abi code.
        rust_entry(arg as *const _)
    }

    /// Entry point for Rust code wishing to start from rt0.
    ///
    /// # Safety
    /// Do not call this unless you are bootstrapping a runtime.
    pub unsafe fn rust_entry(arg: *const RuntimeInfo) -> ! {
        // All we need to do is grab the runtime and call its init function. We want to
        // do as little as possible here.
        twizzler_rt_abi::core::twz_rt_runtime_entry(arg, std_entry_from_runtime)
    }

    extern "C-unwind" {
        fn std_entry_from_runtime(aux: BasicAux) -> BasicReturn;
    }
}
