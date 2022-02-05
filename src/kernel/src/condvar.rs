use alloc::vec::Vec;

use crate::{
    sched::schedule_thread,
    spinlock::{SpinLockGuard, Spinlock},
    thread::{current_thread_ref, ThreadRef},
};

struct InnerCondVar {
    queue: Vec<ThreadRef>,
}

struct CondVar {
    inner: Spinlock<InnerCondVar>,
}

impl CondVar {
    pub fn wait<'a, T>(&self, guard: SpinLockGuard<'a, T>) -> SpinLockGuard<'a, T> {
        crate::interrupt::with_disabled(|| {
            let current_thread = current_thread_ref().unwrap();
            let mut inner = self.inner.lock();
            inner.queue.push(current_thread);
            drop(inner);
            unsafe {
                guard.force_unlock();
                crate::sched::schedule(false);
                guard.force_relock()
            }
        })
    }

    pub fn signal(&self) {
        let mut inner = self.inner.lock();
        while let Some(t) = inner.queue.pop() {
            schedule_thread(t);
        }
    }
}
