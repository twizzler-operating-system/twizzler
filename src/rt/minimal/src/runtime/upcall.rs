//! Implements the non-arch-specific upcall handling functionality for the runtime.

use core::sync::atomic::{AtomicBool, Ordering};

use twizzler_abi::upcall::{UpcallData, UpcallFrame};

#[thread_local]
static UPCALL_PANIC: AtomicBool = AtomicBool::new(false);

#[allow(dead_code)]
pub(crate) fn upcall_rust_entry(frame: &UpcallFrame, info: &UpcallData) {
    if UPCALL_PANIC.load(Ordering::SeqCst) {
        twizzler_abi::syscall::sys_thread_exit(127);
    }
    UPCALL_PANIC.store(true, Ordering::SeqCst);
    #[cfg(target_arch = "aarch64")]
    walk_frames(frame);
    #[cfg(target_arch = "aarch64")]
    panic!(
        "upcall ip={:x} sp={:x} lr={:x} fp={:x} :: {:?}",
        frame.ip(),
        frame.sp(),
        frame.x29,
        frame.fp,
        info
    );
    #[cfg(not(target_arch = "aarch64"))]
    panic!(
        "upcall ip={:x} sp={:x} :: {:?}",
        frame.ip(),
        frame.sp(),
        info
    );
}

/// Print the faulting thread's frame-pointer chain, tagging each return address with the object
/// mapped at its slot so it can be symbolized offline. The kernel stores x29 in `x29` and x30 in
/// `fp`.
#[cfg(target_arch = "aarch64")]
fn walk_frames(frame: &UpcallFrame) {
    twizzler_abi::klog_println!(
        "  x0={:x} x1={:x} x2={:x} x3={:x} x8={:x} x19={:x} x20={:x} x21={:x}",
        frame.x0,
        frame.x1,
        frame.x2,
        frame.x3,
        frame.x8,
        frame.x19,
        frame.x20,
        frame.x21
    );
    let mut fp = frame.x29 as usize;
    for i in 0..32 {
        if fp == 0 || fp & 7 != 0 {
            break;
        }
        let (prev, lr) = unsafe { (*(fp as *const usize), *((fp + 8) as *const usize)) };
        let slot = lr >> 30;
        let id = twizzler_abi::syscall::sys_object_read_map(None, slot)
            .map(|m| m.id)
            .unwrap_or(twizzler_abi::object::ObjID::new(0));
        twizzler_abi::klog_println!("  frame {}: lr={:x} slot={} obj={:?}", i, lr, slot, id);
        if prev <= fp {
            break;
        }
        fp = prev;
    }
}
