//! Implements the core runtime functions.

use dynlink::{
    context::runtime::{RuntimeInitFlags, RuntimeInitInfo},
    library::CtorInfo,
};
use twizzler_abi::upcall::{UpcallFlags, UpcallInfo, UpcallMode, UpcallOptions, UpcallTarget};
use twizzler_runtime_api::{AuxEntry, BasicAux, CoreRuntime};

use crate::{
    preinit::{preinit_abort, preinit_unwrap},
    preinit_println,
    runtime::RuntimeState,
};

use super::{slot::mark_slot_reserved, ReferenceRuntime};

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
        let init_info = preinit_unwrap(aux_slice.iter().find_map(|aux| match aux {
            twizzler_runtime_api::AuxEntry::RuntimeInfo(info) => Some(*info),
            _ => None,
        }));
        let init_info = unsafe { preinit_unwrap((init_info as *const RuntimeInitInfo).as_ref()) };

        if init_info.flags.contains(RuntimeInitFlags::IS_MONITOR) {
            self.set_is_monitor();
        }

        // Step 3: bootstrap pre-std stuff: upcalls, allocator, TLS, constructors (the order matters, ctors need to happen last)
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
        self.init_allocator(init_info);
        self.init_tls(init_info);
        self.init_ctors(&init_info.ctors);

        // Step 4: call into libstd to finish setting up the standard library and call main
        let ba = build_basic_aux(aux_slice);

        let ret = unsafe { std_entry(ba) };
        self.exit(ret.code);
    }

    fn pre_main_hook(&self) {
        self.init_slots();
        self.set_runtime_ready();
    }

    fn post_main_hook(&self) {}
}

impl ReferenceRuntime {
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
    }

    fn init_tls(&self, info: &RuntimeInitInfo) {
        let tls = info.tls_region.get_thread_pointer_value();
        twizzler_abi::syscall::sys_thread_settls(tls as u64);
    }
}
