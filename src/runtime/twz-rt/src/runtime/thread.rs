use twizzler_runtime_api::ThreadRuntime;

use super::ReferenceRuntime;

impl ThreadRuntime for ReferenceRuntime {
    fn available_parallelism(&self) -> std::num::NonZeroUsize {
        todo!()
    }

    fn futex_wait(
        &self,
        futex: &std::sync::atomic::AtomicU32,
        expected: u32,
        timeout: Option<std::time::Duration>,
    ) -> bool {
        todo!()
    }

    fn futex_wake(&self, futex: &std::sync::atomic::AtomicU32) -> bool {
        todo!()
    }

    fn futex_wake_all(&self, futex: &std::sync::atomic::AtomicU32) {
        todo!()
    }

    fn spawn(
        &self,
        args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        todo!()
    }

    fn yield_now(&self) {
        todo!()
    }

    fn set_name(&self, name: &std::ffi::CStr) {
        todo!()
    }

    fn sleep(&self, duration: std::time::Duration) {
        todo!()
    }

    fn join(
        &self,
        id: u32,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), twizzler_runtime_api::JoinError> {
        todo!()
    }

    fn tls_get_addr(&self, tls_index: &twizzler_runtime_api::TlsIndex) -> *const u8 {
        todo!()
    }
}
