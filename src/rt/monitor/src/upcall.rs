use std::{
    ffi::c_void,
    sync::atomic::{AtomicBool, Ordering},
};

use twizzler_abi::upcall::{UpcallData, UpcallFrame, UpcallHandlerFlags};

use crate::mon::get_monitor;
#[thread_local]
static IN_UPCALL_HANDLER: AtomicBool = AtomicBool::new(false);

/// Exit code for a thread killed by an unhandled fault in its own compartment.
const UPCALL_EXIT_CODE: u64 = 137;

/// Report that this thread is about to die while holding a monitor lock.
///
/// An upcall is delivered on the faulting thread, and `sys_thread_exit` unwinds nothing, so a
/// guard taken before the fault is never dropped and the lock is orphaned for the life of the
/// system. Every later compartment load then blocks, with no panic and no output -- the wedge
/// reads as a hang in whichever test happened to be running, which is how it was attributed to
/// `net_test` (and before that to `lltest`) rather than to the monitor.
///
/// This does not save the lock; nothing here can, since a `happylock` guard is owned by the
/// holder's stack. It makes the moment greppable and names the hold site, which is the thing an
/// unattributed five-minute timeout does not tell you.
fn note_dying_with_lock(frame: &UpcallFrame, info: &UpcallData) {
    let Some(site) = crate::lockdiag::current_hold_site() else {
        return;
    };
    twizzler_abi::klog_println!(
        "MONLOCK: FATAL thread {} exiting while holding monitor lock at {}:{} -- lock is now \
         orphaned and every later compartment load will block. fault {:?} at ip {:#x} sp {:#x}",
        info.thread_id,
        site.file(),
        site.line(),
        info.info,
        frame.ip(),
        frame.sp(),
    );
}

pub fn upcall_monitor_handler(frame: &mut UpcallFrame, info: &UpcallData) {
    let _nested = IN_UPCALL_HANDLER.swap(true, Ordering::SeqCst);

    if info.flags.contains(UpcallHandlerFlags::SWITCHED_CONTEXT) {
        let mon = get_monitor();
        match mon.upcall_handle(frame, info) {
            Ok(Some(flags)) => {
                IN_UPCALL_HANDLER.store(false, Ordering::SeqCst);
                unsafe { twizzler_abi::syscall::sys_thread_resume_from_upcall(frame, flags) };
            }
            // Deliberate: the compartment is not being debugged, so the faulting thread is not
            // resumed. Report the faulting frame; capturing the monitor's own backtrace here says
            // nothing about the fault, since it is always the same upcall plumbing.
            Ok(None) => {
                twizzler_abi::klog_println!(
                    "unhandled fault, thread {}: {:?} at ip {:#x} sp {:#x}",
                    info.thread_id,
                    info.info,
                    frame.ip(),
                    frame.sp()
                );
                note_dying_with_lock(frame, info);
                twizzler_abi::syscall::sys_thread_exit(UPCALL_EXIT_CODE);
            }
            Err(e) => {
                twizzler_abi::klog_println!(
                    "monitor upcall handler failed: {:?} {:?} {:?}",
                    e,
                    frame,
                    info
                );
                note_dying_with_lock(frame, info);
                twizzler_abi::syscall::sys_thread_exit(101);
            }
        }
    } else {
        twizzler_abi::klog_println!(
            "monitor got unexpected upcall while in supervisor context: {:?} {:?}",
            frame,
            info
        );
        note_dying_with_lock(frame, info);
        twizzler_abi::syscall::sys_thread_exit(101);
    }
}

pub extern "C-unwind" fn upcall_monitor_handler_entry(frame: *mut c_void, info: *const c_void) {
    unsafe {
        upcall_monitor_handler(
            frame.cast::<UpcallFrame>().as_mut().unwrap(),
            info.cast::<UpcallData>().as_ref().unwrap(),
        );
    }
}
