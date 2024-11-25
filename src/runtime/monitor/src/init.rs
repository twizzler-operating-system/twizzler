use std::ffi::c_void;

use dynlink::context::{runtime::RuntimeInitInfo, Context};

pub(crate) struct InitDynlinkContext {
    pub ctx: *mut Context,
}

impl InitDynlinkContext {
    pub fn get_safe_context(&self) -> &'static mut Context {
        let ctx = self.ctx;
        // Safety: the engine is the only thing that can't cross API boundary coming from bootstrap.
        // Replace it, and we're good to go.
        unsafe {
            (*ctx).replace_engine(Box::new(crate::dlengine::Engine));
            &mut (*ctx)
        }
    }
}

extern "C-unwind" {
    fn __is_monitor() -> Option<*mut c_void>;
}
pub(crate) fn bootstrap_dynlink_context() -> Option<InitDynlinkContext> {
    let info = unsafe {
        __is_monitor()
            .unwrap()
            .cast::<RuntimeInitInfo>()
            .as_mut()
            .unwrap()
    };
    let ctx = info.ctx as *mut Context;

    Some(InitDynlinkContext { ctx })
}

/*
#[no_mangle]
pub unsafe extern "C" fn monitor_entry_from_bootstrap2(rtinfo_ptr: *const RuntimeInfo) {
    let rtinfo = unsafe { rtinfo_ptr.as_ref().unwrap() };
    if rtinfo.kind != RUNTIME_INIT_MONITOR {
        twizzler_abi::klog_println!("cannot initialize monitor without monitor runtime init info");
        twizzler_rt_abi::core::twz_rt_abort();
    }
    let rt_init_info_ptr = rtinfo.init_info.monitor.cast();

    unsafe {
        RTINFO = Some(rt_init_info_ptr);
        twizzler_rt_abi::core::rt0::rust_entry(rtinfo_ptr)
    }
}

/*
#[cfg(target_arch = "x86_64")]
#[naked]
#[no_mangle]
pub unsafe extern "C" fn monitor_entry_from_bootstrap(p: *const RuntimeInfo) -> ! {
    core::arch::naked_asm!("jmp monitor_entry_from_bootstrap2")
}*/

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!("monitor_entry_from_bootstrap: jmp monitor_entry_from_bootstrap2");

*/
/*
#[allow(improper_ctypes)]
extern "C" {
    fn twizzler_call_lang_start(
        main: fn(),
        argc: isize,
        argv: *const *const u8,
        sigpipe: u8,
    ) -> isize;
}

#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn main(argc: i32, argv: *const *const u8) -> i32 {
    //TODO: sigpipe?
    unsafe { twizzler_call_lang_start(crate::main, argc as isize, argv, 0) as i32 }
}

// TODO: we should probably get this for real.
#[cfg(not(test))]
#[no_mangle]
pub extern "C" fn _init() {}

*/
