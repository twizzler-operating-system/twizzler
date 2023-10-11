use twizzler_runtime_api::CoreRuntime;

use crate::preinit_println;

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
        preinit_println!(
            "hello world from refruntime entry, with println {:p} !",
            arg
        );
        loop {}
        todo!()
    }
}
