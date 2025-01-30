use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};
#[thread_local]
static IN_UPCALL_HANDLER: AtomicBool = AtomicBool::new(false);

pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    let nested = IN_UPCALL_HANDLER.swap(true, Ordering::SeqCst);
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        info!("got monitor upcall {:?} {:?}", frame, info);
        loop {}
        // TODO
        if nested {
            twizzler_abi::syscall::sys_thread_exit(101);
        }
    } else {
        twizzler_abi::klog_println!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame,
            info
        );
        twizzler_abi::syscall::sys_thread_exit(101);
    }
    IN_UPCALL_HANDLER.store(nested, Ordering::SeqCst);

    // TODO: we don't always need to exit.
    twizzler_abi::syscall::sys_thread_exit(101);
}

pub extern "C-unwind" fn upcall_monitor_handler_entry(frame: *mut c_void, info: *const c_void) {
    unsafe {
        upcall_monitor_handler(
            frame.cast::<UpcallFrame>().as_mut().unwrap(),
            info.cast::<UpcallData>().as_ref().unwrap(),
        );
    }
}
