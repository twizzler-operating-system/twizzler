use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        info!("got monitor upcall {:?} {:?}", frame, info);
        todo!()
    } else {
        panic!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame, info
        );
    }
}
