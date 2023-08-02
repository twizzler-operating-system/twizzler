use core::sync::atomic::Ordering;

use alloc::{boxed::Box, collections::BTreeMap};
use lazy_static::lazy_static;
use twizzler_abi::{object::ObjID, thread::ExecutionState};

use crate::{
    interrupt::Destination,
    mutex::Mutex,
    processor::ipi_exec,
    sched::{schedule, schedule_resched, schedule_thread},
    thread::current_thread_ref,
};

use super::{
    flags::{THREAD_IS_SUSPENDED, THREAD_MUST_SUSPEND},
    Thread, ThreadRef,
};

lazy_static! {
    static ref SUSPENDED_THREADS: Mutex<BTreeMap<ObjID, ThreadRef>> = Mutex::new(BTreeMap::new());
}

impl Thread {
    /// Tell a thread to suspend. If that thread is the caller, then suspend immediately unless in a critical section.
    /// Otherwise, call out to other CPUs to
    /// force the thread to suspend. Note that if the target is the calling thread, then it will have to be unsuspended before
    /// it returns, and so will NOT be suspended upon completion of this call.
    pub fn suspend(self: &ThreadRef) {
        self.flags.fetch_or(THREAD_MUST_SUSPEND, Ordering::SeqCst);
        if self == &current_thread_ref().unwrap() {
            if !self.is_critical() {
                crate::interrupt::with_disabled(|| {
                    self.maybe_suspend_self();
                });
            }
        } else {
            ipi_exec(Destination::AllButSelf, Box::new(|| schedule_resched()));
        }
    }

    /// Must the thread suspend next chance it gets?
    pub fn must_suspend(&self) -> bool {
        self.flags.load(Ordering::SeqCst) & THREAD_MUST_SUSPEND != 0
    }

    /// Consider suspending ourselves. If someone called [Self::start_suspend], then we will.
    pub fn maybe_suspend_self(self: &ThreadRef) {
        assert_eq!(self.id(), current_thread_ref().unwrap().id());
        if self.flags.load(Ordering::SeqCst) & THREAD_MUST_SUSPEND == 0 {
            return;
        }
        if self.flags.fetch_or(THREAD_IS_SUSPENDED, Ordering::SeqCst) & THREAD_IS_SUSPENDED != 0 {
            // The only time we'll see this flag set is if we are coming out of a suspend. So, just return.
            return;
        }
        {
            // Do this before inserting the thread, to ensure no one writes Running here before we suspend.
            self.set_state(ExecutionState::Suspended);
            let mut suspended_threads = SUSPENDED_THREADS.lock();
            if suspended_threads
                .insert(self.objid(), self.clone())
                .is_some()
            {
                panic!("tried to insert ourselves into suspend list multiple times!");
            }
        }

        // goodnight!
        schedule(false);

        // goodmorning! Clear the flags. This is one operation, so we'll never observe THREAD_IS_SUSPENDED without THREAD_MUST_SUSPEND.
        self.flags.fetch_and(
            !(THREAD_IS_SUSPENDED | THREAD_MUST_SUSPEND),
            Ordering::SeqCst,
        );
    }

    /// If a thread is suspended, then wake it up. Returns false if that thread was not on the suspend list.
    pub fn unsuspend_thread(self: &ThreadRef) -> bool {
        let mut suspended_threads = SUSPENDED_THREADS.lock();
        if suspended_threads.remove(&self.objid()).is_some() {
            // Just throw it on a queue, it'll cleanup its own flag mess.
            schedule_thread(self.clone());
            true
        } else {
            false
        }
    }
}

mod test {
    use core::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    use alloc::sync::Arc;
    use twizzler_kernel_macros::kernel_test;

    use crate::{
        spinlock::Spinlock,
        syscall::sync::sys_thread_sync,
        thread::{entry::run_closure_in_new_thread, priority::Priority},
    };

    #[kernel_test]
    fn test_suspend() {
        // This test is a huge hack, and relies on the system to not schedule
        // threads "badly". But, since we should be the only thread running at this point,
        // it _should_ work correctly.
        let incr = Arc::new(Spinlock::new(0));
        let incr2 = incr.clone();
        let exit_flag = &AtomicBool::default();
        let test_thread = run_closure_in_new_thread(Priority::default_user(), move || loop {
            if exit_flag.load(Ordering::SeqCst) {
                break;
            }
            *incr2.lock() += 1;
        });
        sys_thread_sync(&mut [], Some(&mut Duration::from_secs(1))).unwrap();
        let cur = { *incr.lock() };
        assert_ne!(cur, 0);

        test_thread.0.suspend();
        let cur = { *incr.lock() };
        sys_thread_sync(&mut [], Some(&mut Duration::from_secs(1))).unwrap();
        let cur2 = { *incr.lock() };
        assert_eq!(cur, cur2);
        exit_flag.store(true, Ordering::SeqCst);
        test_thread.0.unsuspend_thread();
        test_thread.1.wait();
    }
}
