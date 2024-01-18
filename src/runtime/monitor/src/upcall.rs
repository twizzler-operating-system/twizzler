use tracing::info;
use twizzler_abi::upcall::{UpcallData, UpcallFrame};

pub fn upcall_monitor_handler(_frame: &mut UpcallFrame, _info: &UpcallData) {
    _frame.rip += 2;
    info!("got monitor upcall");
}
