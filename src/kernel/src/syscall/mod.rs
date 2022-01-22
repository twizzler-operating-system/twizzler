use twizzler_abi::syscall::Syscall;
use x86_64::VirtAddr;

pub trait SyscallContext {
    fn create_jmp_context(target: VirtAddr, stack: VirtAddr, arg: u64) -> Self;
    fn num(&self) -> usize;
    fn arg0<T: From<u64>>(&self) -> T;
    fn arg1<T: From<u64>>(&self) -> T;
    fn arg2<T: From<u64>>(&self) -> T;
    fn arg3<T: From<u64>>(&self) -> T;
    fn arg4<T: From<u64>>(&self) -> T;
    fn arg5<T: From<u64>>(&self) -> T;
    fn pc(&self) -> VirtAddr;
    fn set_return_values<R1, R2>(&mut self, ret0: R1, ret1: R2)
    where
        u64: From<R1>,
        u64: From<R2>;
}

unsafe fn create_user_slice<'a, T>(ptr: u64, len: u64) -> &'a [T] {
    /* TODO: verify pointers */
    core::slice::from_raw_parts(ptr as *const T, len as usize)
}

fn sys_kernel_console_write(data: &[u8], flags: twizzler_abi::syscall::KernelConsoleWriteFlags) {
    let _res = crate::log::write_bytes(data, flags.into());
}

pub fn syscall_entry<T: SyscallContext>(context: &mut T) {
    logln!("syscall! {}", context.num());
    match context.num().into() {
        Syscall::KernelConsoleWrite => {
            let ptr = context.arg0();
            let len = context.arg1();
            let flags =
                twizzler_abi::syscall::KernelConsoleWriteFlags::from_bits_truncate(context.arg2());
            sys_kernel_console_write(unsafe { create_user_slice(ptr, len) }, flags);
        }
        _ => {}
    }
}
