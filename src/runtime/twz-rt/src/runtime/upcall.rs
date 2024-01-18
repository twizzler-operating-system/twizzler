use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

pub(crate) fn upcall_rust_entry(_frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        println!("got supervisor upcall");
    }
    println!("got upcall: {:?}", info);
    panic!("upcall");
}
