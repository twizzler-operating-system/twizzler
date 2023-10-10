use twizzler_runtime_api::CoreRuntime;

use super::ReferenceRuntime;

impl CoreRuntime for ReferenceRuntime {
    fn default_allocator(&self) -> &'static dyn std::alloc::GlobalAlloc {
        todo!()
    }

    fn exit(&self, code: i32) -> ! {
        todo!()
    }

    fn abort(&self) -> ! {
        todo!()
    }

    fn runtime_entry(
        &self,
        arg: *const twizzler_runtime_api::AuxEntry,
        std_entry: unsafe extern "C" fn(
            twizzler_runtime_api::BasicAux,
        ) -> twizzler_runtime_api::BasicReturn,
    ) -> ! {
        twizzler_abi::syscall::sys_kernel_console_write(
            b"hello from refruntime entry\n",
            twizzler_abi::syscall::KernelConsoleWriteFlags::empty(),
        );
        loop {}
        todo!()
    }
}
