use crate::tls::Tcb;

pub(crate) const MINIMUM_TLS_ALIGNMENT: usize = 32;

pub unsafe fn get_thread_control_block<T>() -> *mut Tcb<T> {
    let mut val: usize;
    core::arch::asm!("mov {}, fs:0", out(reg) val);
    val as *mut _
}
