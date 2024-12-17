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
    // Defined by the runtime. Returns a pointer to the runtime init info struct if the runtime is
    // in monitor mode, otherwise returns null.
    fn __is_monitor() -> *mut c_void;
}
pub(crate) fn bootstrap_dynlink_context() -> Option<InitDynlinkContext> {
    let info = unsafe { __is_monitor().cast::<RuntimeInitInfo>().as_mut().unwrap() };
    let ctx = info.ctx as *mut Context;

    Some(InitDynlinkContext { ctx })
}
