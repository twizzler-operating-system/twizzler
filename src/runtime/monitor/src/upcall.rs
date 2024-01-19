use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame};

pub fn upcall_monitor_handler(_frame: &mut UpcallFrame, _info: &UpcallData) {
    info!("got monitor upcall {:?} {:?}", _frame, _info);
    _frame.rip += 2;
}
