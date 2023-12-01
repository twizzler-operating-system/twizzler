use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

pub(crate) fn upcall_rust_entry(frame: &UpcallFrame, info: &UpcallInfo) {
    println!("got upcall: {:?} {:?}", info, frame);
}
