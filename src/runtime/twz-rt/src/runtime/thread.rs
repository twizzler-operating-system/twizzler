use dynlink::tls::Tcb;
use twizzler_abi::syscall::{
    sys_thread_sync, sys_thread_yield, ThreadSync, ThreadSyncError, ThreadSyncFlags, ThreadSyncOp,
    ThreadSyncReference, ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_runtime_api::ThreadRuntime;

use crate::preinit_println;

use super::ReferenceRuntime;

pub struct RuntimeThreadControl {
    name: String,
}

// TODO: implement spawning and joining

impl ThreadRuntime for ReferenceRuntime {
    fn available_parallelism(&self) -> core::num::NonZeroUsize {
        twizzler_abi::syscall::sys_info().cpu_count()
    }

    fn futex_wait(
        &self,
        futex: &core::sync::atomic::AtomicU32,
        expected: u32,
        timeout: Option<core::time::Duration>,
    ) -> bool {
        preinit_println!("AAAAA");
        // No need to wait if the value already changed.
        if futex.load(core::sync::atomic::Ordering::Relaxed) != expected {
            return true;
        }

        let r = sys_thread_sync(
            &mut [ThreadSync::new_sleep(ThreadSyncSleep::new(
                ThreadSyncReference::Virtual32(futex),
                expected as u64,
                ThreadSyncOp::Equal,
                ThreadSyncFlags::empty(),
            ))],
            timeout,
        );

        match r {
            Err(ThreadSyncError::Timeout) => return false,
            _ => return true,
        }
    }

    fn futex_wake(&self, futex: &core::sync::atomic::AtomicU32) -> bool {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            1,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
        // TODO
        false
    }

    fn futex_wake_all(&self, futex: &core::sync::atomic::AtomicU32) {
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual32(futex),
            usize::MAX,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
    }

    fn spawn(
        &self,
        _args: twizzler_runtime_api::ThreadSpawnArgs,
    ) -> Result<u32, twizzler_runtime_api::SpawnError> {
        todo!()
    }

    fn yield_now(&self) {
        sys_thread_yield()
    }

    fn set_name(&self, name: &std::ffi::CStr) {
        let tp: &mut Tcb<RuntimeThreadControl> =
            unsafe { dynlink::tls::get_thread_control_block().as_mut().unwrap() };
        tp.runtime_data.name = name.to_string_lossy().to_string();
    }

    fn sleep(&self, duration: std::time::Duration) {
        let _ = sys_thread_sync(&mut [], Some(duration));
    }

    fn join(
        &self,
        _id: u32,
        _timeout: Option<std::time::Duration>,
    ) -> Result<(), twizzler_runtime_api::JoinError> {
        preinit_println!("HERE: join");
        todo!()
    }

    fn tls_get_addr(&self, index: &twizzler_runtime_api::TlsIndex) -> *const u8 {
        let tp: &Tcb<()> = unsafe { dynlink::tls::get_thread_control_block().as_ref().unwrap() };
        tp.get_addr(index)
    }
}
