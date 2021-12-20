use crate::syscall::Syscall;

#[allow(dead_code)]
pub unsafe fn raw_syscall(call: Syscall, args: &[u64]) -> (u64, u64) {
    let a0 = *args.get(0).unwrap_or(&0u64);
    let a1 = *args.get(1).unwrap_or(&0u64);
    let mut a2 = *args.get(2).unwrap_or(&0u64);
    let a3 = *args.get(3).unwrap_or(&0u64);
    let a4 = *args.get(4).unwrap_or(&0u64);
    let a5 = *args.get(5).unwrap_or(&0u64);

    let mut num = call.num();
    asm!("syscall", inout("rax") num, in("rdi") a0, in("rsi") a1, inout("rdx") a2, in("r10") a3, in("r9") a4, in("r8") a5);
    (num, a2)
}
