//! Implements the core runtime functions.

use core::{alloc::GlobalAlloc, ffi::c_char, mem::MaybeUninit, ptr};

use twizzler_abi::{
    syscall::{sys_get_random, GetRandomFlags},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_rt_abi::{
    core::{BasicAux, BasicReturn, RuntimeInfo, RUNTIME_INIT_MIN},
    info::SystemInfo,
    time::Monotonicity,
};

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

impl MinimalRuntime {
    pub fn default_allocator(&self) -> &'static dyn GlobalAlloc {
        &GLOBAL_ALLOCATOR
    }

    pub fn exit(&self, code: i32) -> ! {
        twizzler_abi::syscall::sys_thread_exit(code as u64);
    }

    pub fn abort(&self) -> ! {
        // Unsure why this causes a warning without this.
        #[allow(unused_unsafe)]
        unsafe {
            core::intrinsics::abort()
        };
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
        let mut null_env: [*mut c_char; 4] = [
            b"RUST_BACKTRACE=1\0".as_ptr() as *mut c_char,
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
            process_phdrs(core::slice::from_raw_parts(
                min_init_info.phdrs as *const Phdr,
                min_init_info.nr_phdrs,
            ));
            if !rt_info.envp.is_null() {
                env_ptr = rt_info.envp;
            }
            if !rt_info.args.is_null() {
                arg_ptr = rt_info.args;
                arg_count = rt_info.argc;
            }
        }

        let tls = init_tls();
        if let Some(tls) = tls {
            twizzler_abi::syscall::sys_thread_settls(tls);
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
        twizzler_abi::syscall::sys_thread_set_upcall(upcall_target);

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
        let info = twizzler_abi::syscall::sys_info();
        SystemInfo {
            clock_monotonicity: Monotonicity::Weak.into(),
            available_parallelism: info.cpu_count().into(),
            page_size: info.page_size(),
        }
    }

    pub fn get_random(&self, buf: &mut [MaybeUninit<u8>], flags: GetRandomFlags) -> usize {
        // rarely if ever would fail
        let out = sys_get_random(buf, flags).expect("failed to get randomness from kernel");
        out
    }
}
