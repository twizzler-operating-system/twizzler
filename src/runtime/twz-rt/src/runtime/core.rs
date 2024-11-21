//! Implements the core runtime functions.

use dynlink::context::runtime::RuntimeInitInfo;
use monitor_api::SharedCompConfig;
use twizzler_abi::upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget};
use twizzler_rt_abi::core::{
    BasicAux, BasicReturn, CompartmentInitInfo, CtorSet, RuntimeInfo, RUNTIME_INIT_COMP,
    RUNTIME_INIT_MONITOR,
};

use super::{slot::mark_slot_reserved, thread::TLS_GEN_MGR, ReferenceRuntime};
use crate::{
    preinit::{preinit_abort, preinit_unwrap},
    preinit_println,
    runtime::RuntimeState,
    RuntimeThreadControl,
};

#[thread_local]
static TLS_TEST: usize = 3222;

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

    pub fn runtime_entry(
        &self,
        rtinfo: *const RuntimeInfo,
        std_entry: unsafe extern "C" fn(BasicAux) -> BasicReturn,
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

    pub fn pre_main_hook(&self) {
        preinit_println!("====== {}", TLS_TEST);
        if self.state().contains(RuntimeState::IS_MONITOR) {
            self.init_slots();
        } else {
            unsafe { self.set_runtime_ready() };
        }
    }

    pub fn post_main_hook(&self) {}
}

impl ReferenceRuntime {
    fn init_for_monitor(&self, init_info: &RuntimeInitInfo) {
        let upcall_target = UpcallTarget::new(
            Some(crate::arch::rr_upcall_entry),
            Some(crate::arch::rr_upcall_entry),
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
        let tls = preinit_unwrap(
            preinit_unwrap(TLS_GEN_MGR.lock().ok())
                .get_next_tls_info(None, || RuntimeThreadControl::new(0)),
        );
        twizzler_abi::syscall::sys_thread_settls(tls as u64);

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
