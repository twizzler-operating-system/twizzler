use std::sync::atomic::{AtomicBool, Ordering};

use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};
use twz_rt::preinit_println;

#[thread_local]
static IN_UPCALL_HANDLER: AtomicBool = AtomicBool::new(false);

#[allow(unreachable_code)]
pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    let nested = IN_UPCALL_HANDLER.swap(true, Ordering::SeqCst);
    // TODO: fix upcall stack trace
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        info!("got monitor upcall {:?} {:?}", frame, info);
        if nested {
            twizzler_abi::syscall::sys_thread_exit(101);
        }
        todo!()
    } else {
        preinit_println!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame,
            info
        );
        twizzler_abi::syscall::sys_thread_exit(101);
    }
    IN_UPCALL_HANDLER.store(nested, Ordering::SeqCst);
}
