use crate::syscall::Syscall;

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
pub unsafe fn raw_syscall(_call: Syscall, _args: &[u64]) -> (u64, u64) {
    todo!()
}
