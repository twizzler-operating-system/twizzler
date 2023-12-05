use twizzler_abi::upcall::{UpcallData, UpcallFrame};

pub(crate) fn upcall_rust_entry(_frame: &UpcallFrame, info: &UpcallData) {
    println!("got upcall: {:?}", info);
    panic!("upcall");
}
