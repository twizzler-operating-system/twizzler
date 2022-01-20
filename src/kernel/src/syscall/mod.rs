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

pub fn syscall_entry<T: SyscallContext>(context: &mut T) {
    logln!("syscall! {}", context.num())
}
