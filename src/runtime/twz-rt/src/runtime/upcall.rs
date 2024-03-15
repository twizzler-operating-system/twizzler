use std::sync::OnceLock;

use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

pub(crate) fn upcall_rust_entry(frame: &mut UpcallFrame, info: &UpcallData) {
    let imp = UPCALL_IMPL.get();
    if let Some(imp) = imp {
        imp(frame, info)
    } else {
        upcall_def_handler(frame, info)
    }
}

pub type HandlerType = &'static (dyn Fn(&mut UpcallFrame, &UpcallData) + Send + Sync + 'static);
static UPCALL_IMPL: OnceLock<HandlerType> = OnceLock::new();

pub fn set_upcall_handler(handler: HandlerType) -> Result<(), HandlerSetError> {
    UPCALL_IMPL.set(handler).map_err(|_| HandlerSetError)
}

#[derive(Clone, Copy, Debug)]
pub struct HandlerSetError;

pub(crate) fn upcall_def_handler(_frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        println!("got supervisor upcall");
    }
    println!("got upcall: {:?}", info);
    panic!("upcall");
}
