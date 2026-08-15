use std::{ffi::c_void, sync::OnceLock};

use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

pub(crate) fn upcall_rust_entry(frame: &mut UpcallFrame, info: &UpcallData) {
    let imp = UPCALL_IMPL.get();
    if let Some(Some(imp)) = imp {
        unsafe {
            imp(
                frame as *mut _ as *mut c_void,
                info as *const _ as *const c_void,
            )
        }
    } else {
        upcall_def_handler(frame, info)
    }
}

pub type HandlerType = unsafe extern "C-unwind" fn(frame: *mut c_void, info: *const c_void);
static UPCALL_IMPL: OnceLock<Option<HandlerType>> = OnceLock::new();

pub fn set_upcall_handler(handler: Option<HandlerType>) -> Result<(), HandlerSetError> {
    UPCALL_IMPL.set(handler).map_err(|_| HandlerSetError)
}

#[derive(Clone, Copy, Debug)]
pub struct HandlerSetError;

/// What an unhandled signal does to this compartment.
///
/// This is the only copy of that table. It is called from [upcall_def_handler] below when libc has
/// no disposition installed, and -- as a weak symbol -- from mlibc when it raises a signal against
/// this process itself (`raise`/`abort`/`kill` of self), where there is no upcall to fall back to.
#[no_mangle]
pub extern "C" fn __twz_rt_default_signal_action(sig: i32) {
    match sig {
        // A status request; there is nothing to report yet. A dump would want the upcall frame,
        // so it belongs in upcall_def_handler rather than here.
        libc::SIGINFO
        // There is no job control to stop a compartment with, so ^Z is ignored rather than fatal.
        | libc::SIGTSTP
        | libc::SIGWINCH
        | libc::SIGCHLD
        | libc::SIGCONT
        | libc::SIGURG => {}
        libc::SIGINT => {
            twizzler_abi::klog_println!("interrupted");
            twizzler_abi::syscall::sys_thread_exit(128 + sig as u64);
        }
        _ => twizzler_abi::syscall::sys_thread_exit(128 + sig as u64),
    }
}

extern "C" {
    #[linkage = "extern_weak"]
    static __mlibc_handle_signal: *mut u8;
}

/// Values returned by mlibc's `__mlibc_handle_signal`. Mirrored from
/// `sysdeps/twizzler/sysdeps.cpp` in the mlibc tree -- keep the two in sync.
const MLIBC_SIGNAL_HANDLED: i32 = 1;
const MLIBC_SIGNAL_IGNORED: i32 = 2;

/// Offer a mailbox signal to libc's disposition table.
///
/// Twizzler keeps no in-kernel signal dispositions, so `sigaction` state lives in mlibc. Resolved
/// weakly: a compartment that doesn't link mlibc has no table to consult, and falls through to
/// [__twz_rt_default_signal_action]. Returns true if libc dealt with the signal (ran a handler,
/// queued it behind a mask, or was explicitly told to ignore it).
fn mlibc_handled_signal(sig: u64) -> bool {
    let handler = unsafe { __mlibc_handle_signal };
    if handler.is_null() {
        return false;
    }
    let handler = unsafe { core::mem::transmute::<_, extern "C" fn(i32) -> i32>(handler) };
    matches!(
        handler(sig as i32),
        MLIBC_SIGNAL_HANDLED | MLIBC_SIGNAL_IGNORED
    )
}

pub(crate) fn upcall_def_handler(_frame: &mut UpcallFrame, info: &UpcallData) {
    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        twizzler_abi::klog_println!("got supervisor upcall");
    }
    match info.info {
        twizzler_abi::upcall::UpcallInfo::Mailbox(sig) => {
            if !mlibc_handled_signal(sig) {
                __twz_rt_default_signal_action(sig as i32);
            }
        }
        _ => {
            panic!("unexpected supervisor upcall in runtime: {:?}", info);
        }
    }
}
