use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

use crate::mon::get_monitor;
#[thread_local]
static IN_UPCALL_HANDLER: AtomicBool = AtomicBool::new(false);

pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    let nested = IN_UPCALL_HANDLER.swap(true, Ordering::SeqCst);
    if nested {}

    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        info!("got monitor upcall {:?} {:?}", frame, info);
        let mon = get_monitor();
        match mon.upcall_handle(frame, info) {
            Ok(flags) => {
                IN_UPCALL_HANDLER.store(false, Ordering::SeqCst);
                unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(frame, flags) };
            }
            Err(_) => {
                twizzler_abi::syscall::sys_thread_exit(101);
            }
        }
    } else {
        twizzler_abi::klog_println!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame,
            info
        );
        twizzler_abi::syscall::sys_thread_exit(101);
    }
}

pub extern "C-unwind" fn upcall_monitor_handler_entry(frame: *mut c_void, info: *const c_void) {
    unsafe {
        upcall_monitor_handler(
            frame.cast::<UpcallFrame>().as_mut().unwrap(),
            info.cast::<UpcallData>().as_ref().unwrap(),
        );
    }
}
