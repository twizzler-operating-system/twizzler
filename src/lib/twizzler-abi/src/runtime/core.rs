use core::alloc::GlobalAlloc;

use twizzler_runtime_api::CoreRuntime;

use super::{alloc::MinimalAllocator, MinimalRuntime};

static GLOBAL_ALLOCATOR: MinimalAllocator = MinimalAllocator::new();

impl CoreRuntime for MinimalRuntime {
    fn default_allocator(&self) -> &'static dyn GlobalAlloc {
        &GLOBAL_ALLOCATOR
    }

    fn exit(&self, code: i32) -> ! {
        crate::syscall::sys_thread_exit(code as u64);
    }

    fn abort(&self) -> ! {
        core::intrinsics::abort();
    }
}

static CORE_R: MinimalRuntime = MinimalRuntime {};

pub fn runtime() -> &'static (dyn CoreRuntime + Sync) {
    &CORE_R
}
