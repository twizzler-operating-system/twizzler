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

fn print_status(_frame: &mut UpcallFrame) {
    //tracing::info!("status request");
}

pub(crate) fn upcall_def_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        twizzler_abi::klog_println!("got supervisor upcall");
    }
    match info.info {
        twizzler_abi::upcall::UpcallInfo::Mailbox(sig) => match sig as i32 {
            libc::SIGINFO => {
                print_status(frame);
            }
            libc::SIGINT => {
                twizzler_abi::klog_println!("interrupted");
                twizzler_abi::syscall::sys_thread_exit(128 + sig);
            }
            _ => twizzler_abi::syscall::sys_thread_exit(128 + sig),
        },
        _ => {
            panic!("unexpected supervisor upcall in runtime: {:?}", info);
        }
    }
}
