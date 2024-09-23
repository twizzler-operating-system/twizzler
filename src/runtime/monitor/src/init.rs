use dynlink::context::{runtime::RuntimeInitInfo, Context};
use twizzler_runtime_api::AuxEntry;

static mut AUX: Option<*const RuntimeInitInfo> = None;

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
    let info = unsafe { AUX.unwrap().as_ref().unwrap() };
    let ctx = info.ctx as *mut Context;
    let root = info.root_name.clone();

    Some(InitDynlinkContext { ctx, root })
}

#[no_mangle]
pub unsafe extern "C" fn monitor_entry_from_bootstrap(aux: *const AuxEntry) {
    let aux_len = unsafe {
        let mut count = 0;
        let mut tmp = aux;
        while !tmp.is_null() {
            if tmp.as_ref().unwrap_unchecked() == &AuxEntry::Null {
                break;
            }
            tmp = tmp.add(1);
            count += 1;
        }
        count
    };
    let aux_slice = if aux.is_null() || aux_len == 0 {
        unsafe { twizzler_runtime_api::rt0::rust_entry(aux) };
    } else {
        unsafe { core::slice::from_raw_parts(aux, aux_len) }
    };
    let runtime_info = aux_slice.iter().find_map(|x| match x {
        AuxEntry::RuntimeInfo(r, 0) => Some(*r),
        _ => None,
    });

    unsafe {
        AUX = runtime_info.map(|info| info as *const RuntimeInitInfo);
        twizzler_runtime_api::rt0::rust_entry(aux);
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
