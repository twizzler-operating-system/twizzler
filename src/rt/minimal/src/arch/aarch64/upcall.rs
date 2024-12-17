#[allow(unused_imports)]
use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallInfo};

#[no_mangle]
pub(crate) unsafe extern "C" fn upcall_entry2(
    frame: *mut UpcallFrame,
    data: *const UpcallData,
) -> ! {
    crate::runtime::upcall::upcall_rust_entry(&*frame, &*data);
    twizzler_rt_abi::core::twz_rt_abort();
}

#[no_mangle]
pub(crate) unsafe extern "C-unwind" fn upcall_entry(
    frame: *mut core::ffi::c_void,
    data: *const core::ffi::c_void,
) -> ! {
    core::arch::asm!(
        "b upcall_entry2",
        in("x0") frame,
        in("x1") data,
        options(noreturn)
    );
}
