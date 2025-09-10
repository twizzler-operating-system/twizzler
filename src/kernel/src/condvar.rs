use core::time::Duration;

use intrusive_collections::{intrusive_adapter, KeyAdapter, RBTree};
use twizzler_abi::{
    object::ObjID,
    syscall::{ThreadSync, ThreadSyncSleep},
};
use twizzler_rt_abi::error::TwzError;

use crate::{
    spinlock::{LockGuard, RelaxStrategy, Spinlock},
    syscall::sync::{add_to_requeue, requeue_all, sys_thread_sync},
    thread::{current_thread_ref, Thread, ThreadRef},
};

struct InnerCondVar {
    queue: RBTree<CondVarLinkAdapter>,
}

intrusive_adapter!(pub CondVarLinkAdapter = ThreadRef: Thread { condvar_link: intrusive_collections::rbtree::AtomicLink });

impl<'a> KeyAdapter<'a> for CondVarLinkAdapter {
    type Key = ObjID;
    fn get_key(&self, s: &'a Thread) -> ObjID {
        s.objid()
    }
}
pub struct CondVar {
    inner: Spinlock<InnerCondVar>,
}

impl CondVar {
    pub const fn new() -> Self {
        Self {
            inner: Spinlock::new(InnerCondVar {
                queue: RBTree::new(CondVarLinkAdapter::NEW),
            }),
        }
    }

    #[track_caller]
    pub fn wait_waiters<'a, T, R: RelaxStrategy>(
        &self,
        mut guard: LockGuard<'a, T, R>,
        mut timeout: Option<Duration>,
        waiter: Option<ThreadSyncSleep>,
    ) -> (LockGuard<'a, T, R>, bool) {
        if waiter.is_none() && timeout.is_none() {
            return (self.wait(guard), false);
        }
        let current_thread =
            current_thread_ref().expect("cannot call wait before threading is enabled");
        let mut inner = self.inner.lock();
        inner.queue.insert(current_thread.clone());
        drop(inner);

        unsafe { guard.force_unlock() };
        let mut to = false;
        if let Some(waiter) = waiter {
            let _ = sys_thread_sync(&mut [ThreadSync::new_sleep(waiter)], timeout.as_mut())
                .inspect_err(|e| {
                    if *e != TwzError::TIMED_OUT {
                        log::warn!("thread sync error in kernel-cv wait");
                    } else {
                        to = true;
                    }
                });
        } else {
            let _ = sys_thread_sync(&mut [], timeout.as_mut()).inspect_err(|e| {
                if *e != TwzError::TIMED_OUT {
                    log::warn!("thread sync error in kernel-cv wait");
                } else {
                    to = true;
                }
            });
        }
        let res = unsafe { guard.force_relock() };
        let current_thread = current_thread_ref().unwrap();
        let mut inner = self.inner.lock();
        inner.queue.find_mut(&current_thread.objid()).remove();
        drop(inner);
        (res, to)
    }

    #[track_caller]
    pub fn wait<'a, T, R: RelaxStrategy>(
        &self,
        mut guard: LockGuard<'a, T, R>,
    ) -> LockGuard<'a, T, R> {
        let current_thread =
            current_thread_ref().expect("cannot call wait before threading is enabled");
        let mut inner = self.inner.lock();
        inner.queue.insert(current_thread.clone());
        drop(inner);
        let critical_guard = current_thread.enter_critical();
        current_thread.set_sync_sleep();
        current_thread.set_sync_sleep_done();
        let res = unsafe {
            guard.force_unlock();
            crate::syscall::sync::finish_blocking(critical_guard);
            guard.force_relock()
        };
        let current_thread = current_thread_ref().unwrap();
        let mut inner = self.inner.lock();
        inner.queue.find_mut(&current_thread.objid()).remove();
        drop(inner);
        current_thread.reset_sync_sleep();
        current_thread.reset_sync_sleep_done();
        res
    }

    pub fn signal(&self) {
        const MAX_PER_ITER: usize = 8;
        let mut threads_to_wake = heapless::Vec::<_, MAX_PER_ITER>::new();
        loop {
            let mut inner = self.inner.lock();
            if inner.queue.is_empty() {
                break;
            }
            let mut node = inner.queue.front_mut();
            while !threads_to_wake.is_full() && !node.is_null() {
                if node.get().unwrap().reset_sync_sleep() {
                    // Safety: vec isn't full, checked above.
                    unsafe { threads_to_wake.push_unchecked(node.remove().unwrap()) };
                } else {
                    node.move_next();
                }
            }

            drop(inner);
            for t in threads_to_wake.drain(..) {
                add_to_requeue(t);
            }
        }
        requeue_all();
    }

    pub fn has_waiters(&self) -> bool {
        !self.inner.lock().queue.is_empty()
    }
}

impl Drop for CondVar {
    fn drop(&mut self) {
        self.signal()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::time::Duration;

    use twizzler_kernel_macros::kernel_test;

    use super::CondVar;
    use crate::{
        spinlock::ReschedulingSpinlock,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
    };

    #[kernel_test]
    fn test_condvar() {
        let lock = Arc::new(ReschedulingSpinlock::new(0));
        let cv = Arc::new(CondVar::new());
        let cv2 = cv.clone();
        let lock2 = lock.clone();

        const ITERS: usize = 500;
        for i in 0..ITERS {
            log!(".");
            {
                *lock.lock() = 0;
            }
            let handle = run_closure_in_new_thread(Priority::REALTIME, || {
                if i % 3 == 0 {
                    let _ = crate::syscall::sync::sys_thread_sync(
                        &mut [],
                        Some(&mut Duration::from_millis(1)),
                    );
                }
                let mut inner = lock.lock();
                *inner += 1;
                drop(inner);
                cv.signal();
            });

            if i % 5 == 0 {
                let _ = crate::syscall::sync::sys_thread_sync(
                    &mut [],
                    Some(&mut Duration::from_millis(1)),
                );
            }
            'inner: loop {
                let inner = lock2.lock();
                if *inner != 0 {
                    break 'inner;
                }
                cv2.wait(inner);
            }
            handle.1.wait();
        }
    }
}
