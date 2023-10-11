use twizzler_runtime_api::ThreadRuntime;

use super::ReferenceRuntime;

impl ThreadRuntime for ReferenceRuntime {
    fn available_parallelism(&self) -> std::num::NonZeroUsize {
        todo!()
    }

    fn futex_wait(
        &self,
        _futex: &std::sync::atomic::AtomicU32,
        _expected: u32,
        _timeout: Option<std::time::Duration>,
    ) -> bool {
        todo!()
    }

    fn futex_wake(&self, _futex: &std::sync::atomic::AtomicU32) -> bool {
        todo!()
    }

    fn futex_wake_all(&self, _futex: &std::sync::atomic::AtomicU32) {
        todo!()
    }

    fn spawn(
        &self,
        _args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        todo!()
    }

    fn yield_now(&self) {
        todo!()
    }

    fn set_name(&self, _name: &std::ffi::CStr) {
        todo!()
    }

    fn sleep(&self, _duration: std::time::Duration) {
        todo!()
    }

    fn join(
        &self,
        _id: u32,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), twizzler_runtime_api::JoinError> {
        todo!()
    }

    fn tls_get_addr(&self, _tls_index: &twizzler_runtime_api::TlsIndex) -> *const u8 {
        todo!()
    }
}
