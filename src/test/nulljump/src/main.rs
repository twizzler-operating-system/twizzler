//! Faults through address 0: a jump to it (default) or a read of it (`nulljump read`) must each
//! reach the process as a fault upcall, never run code or panic the kernel.
extern crate twizzler_runtime;

use twizzler_abi::klog_println;

fn main() {
    let p = std::hint::black_box(0usize) as *const u64;
    if std::env::args().nth(1).as_deref() == Some("read") {
        klog_println!("nulljump: reading 0");
        let v = unsafe { core::ptr::read_volatile(p) };
        klog_println!("nulljump: *0 = {:#x} (a null page must not be readable)", v);
        return;
    }
    klog_println!("nulljump: jumping to 0");
    let f: extern "C" fn() = unsafe { core::mem::transmute(p) };
    f();
    klog_println!("nulljump: returned?!");
}
