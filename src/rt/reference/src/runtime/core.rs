//! Implements the core runtime functions.

use core::mem::MaybeUninit;
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_void, CStr, CString},
    sync::{Mutex, OnceLock},
};

use dynlink::context::runtime::RuntimeInitInfo;
use monitor_api::{RuntimeThreadControl, SharedCompConfig};
use secgate::SecGateReturn;
use tracing::Level;
use twizzler_abi::{
    syscall::{sys_get_random, GetRandomFlags},
    upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget},
};
use twizzler_rt_abi::{
    core::{
        BasicAux, BasicReturn, CompartmentInitInfo, CtorSet, ExitCode, RuntimeInfo,
        RUNTIME_INIT_COMP, RUNTIME_INIT_MONITOR,
    },
    info::SystemInfo,
    time::Monotonicity,
};

use super::{slot::mark_slot_reserved, thread::TLS_GEN_MGR, ReferenceRuntime};
use crate::{
    preinit::{preinit_abort, preinit_unwrap},
    preinit_println,
    runtime::RuntimeState,
};

#[derive(Copy, Clone)]
struct PtrToInfo(*mut c_void);
unsafe impl Send for PtrToInfo {}
unsafe impl Sync for PtrToInfo {}
static MON_RTINFO: OnceLock<Option<PtrToInfo>> = OnceLock::new();

impl ReferenceRuntime {
    pub fn default_allocator(&self) -> &'static dyn std::alloc::GlobalAlloc {
        self.get_alloc()
    }

    pub fn exit(&self, code: i32) -> ! {
        if self.state().contains(RuntimeState::READY) {
            twizzler_abi::syscall::sys_thread_exit(code as u64);
        } else {
            preinit_println!("runtime exit before runtime ready: {}", code);
            preinit_abort();
        }
    }

    pub fn abort(&self) -> ! {
        if self.state().contains(RuntimeState::READY) {
            preinit_abort();
        } else {
            preinit_println!("runtime abort before runtime ready");
            preinit_abort();
        }
    }

    pub fn is_monitor(&self) -> Option<*mut c_void> {
        MON_RTINFO
            .get()
            .as_ref()
            .unwrap()
            .map(|x| x.0 as *mut _ as *mut c_void)
    }

    pub fn cgetenv(&self, name: &CStr) -> *const c_char {
        // TODO: this approach is very simple, but it leaks if the environment changes a lot.
        static ENVMAP: Mutex<BTreeMap<String, CString>> = Mutex::new(BTreeMap::new());
        let Ok(name) = name.to_str() else {
            return core::ptr::null();
        };
        let Ok(val) = std::env::var(name) else {
            return core::ptr::null();
        };
        let mut envmap = ENVMAP.lock().unwrap();
        envmap
            .entry(val.to_string())
            .or_insert_with(|| CString::new(val.to_string()).unwrap())
            .as_ptr()
    }

    pub fn runtime_entry(
        &self,
        rtinfo: *const RuntimeInfo,
        std_entry: unsafe extern "C-unwind" fn(BasicAux) -> BasicReturn,
    ) -> ! {
        let rtinfo = unsafe { rtinfo.as_ref().unwrap() };
        match rtinfo.kind {
            RUNTIME_INIT_MONITOR => {
                let init_info = unsafe {
                    rtinfo
                        .init_info
                        .monitor
                        .cast::<RuntimeInitInfo>()
                        .as_ref()
                        .unwrap()
                };
                let _ = MON_RTINFO.set(Some(PtrToInfo(init_info as *const _ as *mut _)));
                self.init_for_monitor(init_info);
            }
            RUNTIME_INIT_COMP => {
                let init_info = unsafe {
                    rtinfo
                        .init_info
                        .comp
                        .cast::<CompartmentInitInfo>()
                        .as_ref()
                        .unwrap()
                };
                let _ = MON_RTINFO.set(None);
                self.init_for_compartment(init_info);
            }
            x => {
                preinit_println!("unsupported runtime kind: {}", x);
                preinit_abort();
            }
        }

        // Step 3: call into libstd to finish setting up the standard library and call main
        let ba = BasicAux {
            argc: rtinfo.argc,
            args: rtinfo.args,
            env: rtinfo.envp,
        };
        let ret = unsafe { std_entry(ba) };
        self.exit(ret.code);
    }

    pub fn pre_main_hook(&self) -> Option<ExitCode> {
        // TODO: control this with env vars
        tracing::subscriber::set_global_default(
            tracing_subscriber::fmt()
                .with_max_level(Level::INFO)
                .finish(),
        )
        .unwrap();
        if self.state().contains(RuntimeState::IS_MONITOR) {
            self.init_slots();
            None
        } else {
            unsafe { self.set_runtime_ready() };
            let ret = match monitor_api::monitor_rt_comp_ctrl(
                monitor_api::MonitorCompControlCmd::RuntimeReady,
            ) {
                SecGateReturn::Success(ret) => ret,
                _ => self.abort(),
            };
            ret
        }
    }

    pub fn post_main_hook(&self) {
        monitor_api::monitor_rt_comp_ctrl(monitor_api::MonitorCompControlCmd::RuntimePostMain);
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
        // TODO: Once the Randomness PR is in, fix this.
        let out = sys_get_random(buf, flags).expect("failed to get randomness from kernel");
        out
    }
}

impl ReferenceRuntime {
    fn init_for_monitor(&self, init_info: &RuntimeInitInfo) {
        let upcall_target = UpcallTarget::new(
            Some(
                twizzler_rt_abi::arch::__twz_rt_upcall_entry
                    as unsafe extern "C-unwind" fn(_, _) -> !,
            ),
            Some(twizzler_rt_abi::arch::__twz_rt_upcall_entry),
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
        self.set_is_monitor();
        self.init_allocator(init_info);
        self.init_tls(init_info);
        self.init_ctors(&init_info.ctors);
    }

    fn init_for_compartment(&self, init_info: &CompartmentInitInfo) {
        unsafe {
            preinit_unwrap(
                monitor_api::set_comp_config(
                    (init_info.comp_config_info as *const SharedCompConfig)
                        .as_ref()
                        .unwrap(),
                )
                .ok(),
            );
        }
        let mut tg = preinit_unwrap(TLS_GEN_MGR.write().ok());
        let tls = tg.get_next_tls_info(None, || RuntimeThreadControl::new(0));
        twizzler_abi::syscall::sys_thread_settls(preinit_unwrap(tls) as u64);

        if !init_info.ctor_set_array.is_null() && init_info.ctor_set_len != 0 {
            let ctor_slice = unsafe {
                core::slice::from_raw_parts(init_info.ctor_set_array, init_info.ctor_set_len)
            };
            self.init_ctors(ctor_slice);
        }
    }

    fn init_ctors(&self, ctor_array: &[CtorSet]) {
        for ctor in ctor_array {
            unsafe {
                if let Some(legacy_init) = ctor.legacy_init {
                    (core::mem::transmute::<_, extern "C" fn()>(legacy_init))();
                }
                if !ctor.init_array.is_null() && ctor.init_array_len > 0 {
                    let init_slice: &[usize] = core::slice::from_raw_parts(
                        ctor.init_array as *const usize,
                        ctor.init_array_len,
                    );
                    for call in init_slice.iter().cloned() {
                        (core::mem::transmute::<_, extern "C" fn()>(call))();
                    }
                }
            }
        }
    }

    fn init_allocator(&self, info: &RuntimeInitInfo) {
        for slot in &info.used_slots {
            mark_slot_reserved(*slot);
        }
        self.register_bootstrap_alloc(info.bootstrap_alloc_slot);
    }

    fn init_tls(&self, info: &RuntimeInitInfo) {
        let tls = info.tls_region.get_thread_pointer_value();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
    }
}

#[allow(improper_ctypes)]
extern "C" {
    fn twizzler_call_lang_start(
        main: fn(),
        argc: isize,
        argv: *const *const u8,
        sigpipe: u8,
    ) -> isize;
}

#[no_mangle]
#[linkage = "weak"]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    //TODO: sigpipe?
    unsafe { twizzler_call_lang_start(dead_end, argc as isize, argv, 0) as i32 }
}

fn dead_end() {
    twizzler_abi::syscall::sys_thread_exit(0);
}

// TODO: we should probably get this for real.
#[cfg(not(test))]
#[no_mangle]
#[linkage = "weak"]
pub extern "C" fn _init() {}
