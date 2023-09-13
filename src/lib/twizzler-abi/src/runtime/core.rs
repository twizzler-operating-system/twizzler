use twizzler_runtime_api::CoreRuntime;

use super::{alloc::MinimalAllocator, MinimalRuntime};

static GLOBAL_ALLOCATOR: MinimalAllocator = MinimalAllocator::new();

impl CoreRuntime for MinimalRuntime {
    type AllocatorType = MinimalAllocator;

    fn default_allocator(&self) -> &Self::AllocatorType {
        &GLOBAL_ALLOCATOR
    }

    fn exit(&self, code: i32) -> ! {
        crate::syscall::sys_thread_exit(code as u64);
    }

    fn abort(&self) -> ! {
        core::intrinsics::abort();
    }
}
