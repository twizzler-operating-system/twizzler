use crate::syscall::Syscall;

/// Magic number used when calling SVC (syscall)
pub const SYSCALL_MAGIC: u64 = 42;

#[allow(dead_code)]
/// Call into the kernel to perform a synchronous system call. The args array can be at most 6 long,
/// and the meaning of the args depends on the system call.
/// The kernel can return two 64-bit values, whose meaning depends on the system call.
///
/// You probably don't want to call this function directly, and you should instead use the wrappers
/// in [crate::syscall].
///
/// # Safety
/// The caller must ensure that the args have the correct meaning for the syscall in question, and
/// to handle the return values correctly. Additionally, calling the kernel can invoke any kind of
/// weirdness if you do something wrong.
pub unsafe fn raw_syscall(call: Syscall, args: &[u64]) -> (u64, u64) {
    if core::intrinsics::unlikely(args.len() > 6) {
        crate::print_err("too many arguments to raw_syscall");
        crate::internal_abort();
    }
    let a0 = *args.get(0).unwrap_or(&0u64);
    let a1 = *args.get(1).unwrap_or(&0u64);
    let a2 = *args.get(2).unwrap_or(&0u64);
    let a3 = *args.get(3).unwrap_or(&0u64);
    let a4 = *args.get(4).unwrap_or(&0u64);
    let a5 = *args.get(5).unwrap_or(&0u64);

    let mut num = call.num();
    let ret1: u64;
    core::arch::asm!(
        "svc #{magic}",
        magic = const SYSCALL_MAGIC,
        in("x0") a0,
        in("x1") a1,
        in("x2") a2,
        in("x3") a3,
        in("x4") a4,
        in("x5") a5,
        // register x6 is used to store the
        // syscall number, but is overwritten
        // with the first return value
        inout("x6") num,
        out("x7") ret1,
    );
    (num, ret1)
}
