use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};
use twz_rt::preinit_println;

pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    // TODO: fix upcall stack trace
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        info!("got monitor upcall {:?} {:?}", frame, info);
        todo!()
    } else {
        preinit_println!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame,
            info
        );
        twizzler_abi::syscall::sys_thread_exit(101);
    }
}
