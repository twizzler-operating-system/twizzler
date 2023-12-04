use twizzler_abi::upcall::{UpcallFrame, UpcallInfo};

use crate::preinit_println;

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C" fn rr_upcall_entry(
    _frame: *const UpcallFrame,
    _info: *const UpcallInfo,
) -> ! {
    todo!()
}

#[cfg(feature = "runtime")]
#[no_mangle]
pub(crate) unsafe extern "C" fn rr_upcall_entry2(
    frame: *const UpcallFrame,
    info: *const UpcallInfo,
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
