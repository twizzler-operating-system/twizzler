use core::sync::atomic::{AtomicU64, Ordering};

use crate::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};

pub struct Mutex {
    lock: AtomicU64,
}

unsafe impl Send for Mutex {}

impl Mutex {
    pub const fn new() -> Mutex {
        Mutex {
            lock: AtomicU64::new(0),
        }
    }

    #[inline]
    pub unsafe fn lock(&self) {
        for _ in 0..100 {
            let result = self
                .lock
                .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst);
            if result.is_ok() {
                return;
            }
            core::hint::spin_loop();
        }
        let _ = self
            .lock
            .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst);
        let sleep = ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&self.lock),
            2,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        ));
        loop {
            let state = self.lock.swap(2, Ordering::SeqCst);
            if state == 0 {
                break;
            }
            let _ = sys_thread_sync(&mut [sleep], None);
        }
    }

    #[inline]
    pub unsafe fn unlock(&self) {
        if self.lock.swap(0, Ordering::SeqCst) == 1 {
            return;
        }
        for _ in 0..200 {
            if self.lock.load(Ordering::SeqCst) > 0 {
                if self
                    .lock
                    .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
                    != Err(0)
                {
                    return;
                }
            }
            core::hint::spin_loop();
        }
        let wake = ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&self.lock),
            1,
        ));
        let _ = sys_thread_sync(&mut [wake], None);
    }

    #[inline]
    pub unsafe fn try_lock(&self) -> bool {
        self.lock
            .compare_exchange_weak(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}
