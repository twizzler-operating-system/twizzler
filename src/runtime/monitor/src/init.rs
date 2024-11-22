use dynlink::context::{runtime::RuntimeInitInfo, Context};
use twizzler_rt_abi::core::{RuntimeInfo, RUNTIME_INIT_MONITOR};
use twz_rt::{preinit::preinit_abort, preinit_println};

static mut RTINFO: Option<*const RuntimeInitInfo> = None;

pub(crate) struct InitDynlinkContext {
    pub ctx: *mut Context,
    pub root: String,
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

pub(crate) fn bootstrap_dynlink_context() -> Option<InitDynlinkContext> {
    let info = unsafe { RTINFO.unwrap().as_ref().unwrap() };
    let ctx = info.ctx as *mut Context;
    let root = info.root_name.clone();

    Some(InitDynlinkContext { ctx, root })
}

#[no_mangle]
pub unsafe extern "C" fn monitor_entry_from_bootstrap(rtinfo_ptr: *const RuntimeInfo) {
    let rtinfo = unsafe { rtinfo_ptr.as_ref().unwrap() };
    if rtinfo.kind != RUNTIME_INIT_MONITOR {
        preinit_println!("cannot initialize monitor without monitor runtime init info");
        preinit_abort();
    }
    let rt_init_info_ptr = rtinfo.init_info.monitor.cast();

    unsafe {
        RTINFO = Some(rt_init_info_ptr);
        twizzler_rt_abi::core::rt0::rust_entry(rtinfo_ptr)
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
