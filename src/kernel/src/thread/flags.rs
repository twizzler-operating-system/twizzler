use core::sync::atomic::Ordering;

use super::{current_thread_ref, Thread};

pub(super) const THREAD_PROC_IDLE: u32 = 1;
pub(super) const THREAD_HAS_DONATED_PRIORITY: u32 = 2;
pub(super) const THREAD_IN_KERNEL: u32 = 4;
pub(super) const THREAD_IS_SYNC_SLEEP: u32 = 8;
pub(super) const THREAD_IS_SYNC_SLEEP_DONE: u32 = 16;
pub(super) const THREAD_IS_EXITING: u32 = 32;
pub(super) const THREAD_IS_SUSPENDED: u32 = 64;
pub(super) const THREAD_MUST_SUSPEND: u32 = 128;

pub fn enter_kernel() {
    if let Some(thread) = current_thread_ref() {
        thread.flags.fetch_or(THREAD_IN_KERNEL, Ordering::SeqCst);
    }
}

pub fn exit_kernel() {
    if let Some(thread) = current_thread_ref() {
        thread.flags.fetch_and(!THREAD_IN_KERNEL, Ordering::SeqCst);
    }
}

impl Thread {
    #[inline]
    pub fn is_idle_thread(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_PROC_IDLE != 0
    }

    #[inline]
    pub fn is_in_user(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IN_KERNEL == 0
    }
    pub fn set_is_exiting(&self) {
        self.flags.fetch_or(THREAD_IS_EXITING, Ordering::SeqCst);
    }

    pub fn is_exiting(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_IS_EXITING != 0
    }

    pub fn set_sync_sleep(&self) {
        self.flags.fetch_or(THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
    }

    pub fn reset_sync_sleep(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP) != 0
    }

    pub fn set_sync_sleep_done(&self) {
        self.flags
            .fetch_or(THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
    }

    pub fn reset_sync_sleep_done(&self) -> bool {
        let old = self
            .flags
            .fetch_and(!THREAD_IS_SYNC_SLEEP_DONE, Ordering::SeqCst);
        (old & THREAD_IS_SYNC_SLEEP_DONE) != 0
    }
}
