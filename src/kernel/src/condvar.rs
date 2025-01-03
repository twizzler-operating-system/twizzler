use alloc::vec::Vec;

use intrusive_collections::{intrusive_adapter, KeyAdapter, RBTree};
use twizzler_abi::{object::ObjID, thread::ExecutionState};

use crate::{
    sched::schedule_thread,
    spinlock::{SpinLockGuard, Spinlock},
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

    pub fn wait<'a, T>(
        &self,
        mut guard: SpinLockGuard<'a, T>,
        istate: bool,
    ) -> SpinLockGuard<'a, T> {
        let current_thread =
            current_thread_ref().expect("cannot call wait before threading is enabled");
        crate::interrupt::set(false);
        let mut inner = self.inner.lock();
        inner.queue.insert(current_thread);
        drop(inner);
        let res = unsafe {
            guard.force_unlock();
            current_thread_ref()
                .unwrap()
                .set_state(ExecutionState::Sleeping);
            crate::sched::schedule(false);
            current_thread_ref()
                .unwrap()
                .set_state(ExecutionState::Running);
            guard.force_relock()
        };
        let current_thread = current_thread_ref().unwrap();
        let mut inner = self.inner.lock();
        inner.queue.find_mut(&current_thread.objid()).remove();
        drop(inner);
        crate::interrupt::set(istate);
        res
    }

    pub fn signal(&self) {
        let mut inner = self.inner.lock();
        let mut node = inner.queue.front_mut();
        let mut threads_to_wake = Vec::new();
        while let Some(t) = node.remove() {
            threads_to_wake.push(t);
        }

        drop(inner);
        for t in threads_to_wake {
            schedule_thread(t);
        }
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
        spinlock::Spinlock,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
    };

    #[kernel_test]
    fn test_condvar() {
        let lock = Arc::new(Spinlock::new(0));
        let cv = Arc::new(CondVar::new());
        let cv2 = cv.clone();
        let lock2 = lock.clone();

        const ITERS: usize = 500;
        for _ in 0..ITERS {
            let handle = run_closure_in_new_thread(Priority::default_user(), || {
                let _ = crate::syscall::sync::sys_thread_sync(
                    &mut [],
                    Some(&mut Duration::from_millis(1)),
                );
                let mut inner = lock.lock();
                *inner += 1;
                cv.signal();
            });

            let _ =
                crate::syscall::sync::sys_thread_sync(&mut [], Some(&mut Duration::from_millis(1)));
            'inner: loop {
                let inner = lock2.lock();
                if *inner != 0 {
                    break 'inner;
                }
                cv2.wait(inner, true);
            }
            handle.1.wait(true);
        }
    }
}
