use twizzler_abi::upcall::{UpcallData, UpcallFrame};

use crate::preinit_println;

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn rr_upcall_entry(
    _frame: *mut UpcallFrame,
    _info: *const UpcallData,
) -> ! {
    todo!()
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn rr_upcall_entry2(
    frame: *mut UpcallFrame,
    info: *const UpcallData,
) -> ! {
    use crate::runtime::do_impl::__twz_get_runtime;

    preinit_println!(
        "got upcall: {:?}, {:?}",
        frame.as_ref().unwrap(),
        info.as_ref().unwrap()
    );
    //crate::runtime::upcall::upcall_rust_entry(&*rdi, &*rsi);
    let runtime = __twz_get_runtime();
    runtime.abort()
}
