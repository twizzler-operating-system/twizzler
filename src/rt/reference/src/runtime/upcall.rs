use std::{ffi::c_void, sync::OnceLock};

use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

pub(crate) fn upcall_rust_entry(frame: &mut UpcallFrame, info: &UpcallData) {
    let imp = UPCALL_IMPL.get();
    if let Some(Some(imp)) = imp {
        unsafe {
            imp(
                frame as *mut _ as *mut c_void,
                info as *const _ as *const c_void,
            )
        }
    } else {
        upcall_def_handler(frame, info)
    }
}

pub type HandlerType = unsafe extern "C-unwind" fn(frame: *mut c_void, info: *const c_void);
static UPCALL_IMPL: OnceLock<Option<HandlerType>> = OnceLock::new();

pub fn set_upcall_handler(handler: Option<HandlerType>) -> Result<(), HandlerSetError> {
    UPCALL_IMPL.set(handler).map_err(|_| HandlerSetError)
}

#[derive(Clone, Copy, Debug)]
pub struct HandlerSetError;

pub(crate) fn upcall_def_handler(_frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        println!("got supervisor upcall");
    }
    println!("got upcall: {:?}", info);
    panic!("upcall");
}
