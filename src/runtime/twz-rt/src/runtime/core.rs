//! Implements the core runtime functions.

use dynlink::{context::runtime::RuntimeInitInfo, library::CtorInfo};
use monitor_api::SharedCompConfig;
use twizzler_abi::upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget};
use twizzler_runtime_api::{AuxEntry, BasicAux, CoreRuntime};

use super::{slot::mark_slot_reserved, thread::TLS_GEN_MGR, ReferenceRuntime};
use crate::{
    preinit::{preinit_abort, preinit_unwrap},
    preinit_println,
    runtime::RuntimeState,
    RuntimeThreadControl,
};

#[repr(C)]
pub struct CompartmentInitInfo {
    pub ctor_array_start: usize,
    pub ctor_array_len: usize,
    pub comp_config_addr: usize,
}

fn build_basic_aux(aux: &[AuxEntry]) -> BasicAux {
    let args = aux
        .iter()
        .find_map(|aux| match aux {
            AuxEntry::Arguments(len, addr) => Some((*len, *addr as usize as *const _)),
            _ => None,
        })
        .unwrap_or((0, core::ptr::null()));

    let env = aux
        .iter()
        .find_map(|aux| match aux {
            AuxEntry::Environment(addr) => Some(*addr as usize as *const _),
            _ => None,
        })
        .unwrap_or(core::ptr::null());

    BasicAux {
        argc: args.0,
        args: args.1,
        env,
    }
}

#[thread_local]
static TLS_TEST: usize = 3222;

impl CoreRuntime for ReferenceRuntime {
    fn default_allocator(&self) -> &'static dyn std::alloc::GlobalAlloc {
        self.get_alloc()
    }

    fn exit(&self, code: i32) -> ! {
        if self.state().contains(RuntimeState::READY) {
            twizzler_abi::syscall::sys_thread_exit(code as u64);
        } else {
            preinit_println!("runtime exit before runtime ready: {}", code);
            preinit_abort();
        }
    }

    fn abort(&self) -> ! {
        if self.state().contains(RuntimeState::READY) {
            preinit_abort();
        } else {
            preinit_println!("runtime abort before runtime ready");
            preinit_abort();
        }
    }

    fn runtime_entry(
        &self,
        aux: *const twizzler_runtime_api::AuxEntry,
        std_entry: unsafe extern "C" fn(
            twizzler_runtime_api::BasicAux,
        ) -> twizzler_runtime_api::BasicReturn,
    ) -> ! {
        twizzler_abi::syscall::sys_kernel_console_write(
            b"here\n",
            twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
        );
        // Step 1: build the aux slice (count until we see a null entry)
        let aux_len = unsafe {
            let mut count = 0;
            let mut tmp = aux;
            while !tmp.is_null() {
                if preinit_unwrap(tmp.as_ref()) == &AuxEntry::Null {
                    break;
                }
                tmp = tmp.add(1);
                count += 1;
            }
            count
        };
        let aux_slice = if aux.is_null() || aux_len == 0 {
            preinit_println!("no AUX info provided");
            preinit_abort();
        } else {
            unsafe { core::slice::from_raw_parts(aux, aux_len) }
        };
        // Step 2: do some early AUX processing
        let (init_info, is_monitor) = preinit_unwrap(aux_slice.iter().find_map(|aux| match aux {
            twizzler_runtime_api::AuxEntry::RuntimeInfo(info, data) => Some((*info, *data == 0)),
            _ => None,
        }));

        if is_monitor {
            let init_info =
                unsafe { preinit_unwrap((init_info as *const RuntimeInitInfo).as_ref()) };
            self.init_for_monitor(init_info);
        } else {
            let init_info =
                unsafe { preinit_unwrap((init_info as *const CompartmentInitInfo).as_ref()) };
            self.init_for_compartment(init_info);
        }
        // Step 3: call into libstd to finish setting up the standard library and call main
        let ba = build_basic_aux(aux_slice);
        let ret = unsafe { std_entry(ba) };
        self.exit(ret.code);
    }

    fn pre_main_hook(&self) {
        if self.state().contains(RuntimeState::IS_MONITOR) {
            self.init_slots();
        }
        self.set_runtime_ready();
    }

    fn post_main_hook(&self) {}
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
                    (init_info.comp_config_addr as *const SharedCompConfig)
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

        if init_info.ctor_array_start != 0 && init_info.ctor_array_len != 0 {
            let ctor_slice = unsafe {
                core::slice::from_raw_parts(
                    init_info.ctor_array_start as *const CtorInfo,
                    init_info.ctor_array_len,
                )
            };
            self.init_ctors(ctor_slice);
        }
    }

    fn init_ctors(&self, ctor_array: &[CtorInfo]) {
        for ctor in ctor_array {
            unsafe {
                if ctor.legacy_init != 0 {
                    (core::mem::transmute::<_, extern "C" fn()>(ctor.legacy_init))();
                }
                if ctor.init_array > 0 && ctor.init_array_len > 0 {
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
