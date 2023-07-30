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
    /// Tell a thread to suspend. If that thread is the caller, then suspend immediately. Otherwise, call out to other CPUs to
    /// force the thread to suspend. In both cases, the thread will be suspended before this call returns (though, in the case of
    /// the thread being the current thread, it will have to be unsuspended before it returns).
    pub fn suspend(self: &ThreadRef) {
        self.flags.fetch_or(THREAD_MUST_SUSPEND, Ordering::SeqCst);
        if self == &current_thread_ref().unwrap() {
            self.maybe_suspend_self();
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
        assert_eq!(self, current_thread_ref().unwrap());
        if self.flags.load(Ordering::SeqCst) & THREAD_MUST_SUSPEND == 0 {
            return;
        }
        if self.flags.fetch_or(THREAD_IS_SUSPENDED, Ordering::SeqCst) & THREAD_IS_SUSPENDED != 0 {
            panic!("we tried to suspend, but we already suspended?");
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
