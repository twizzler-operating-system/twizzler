use std::{
    collections::HashMap,
    marker::PhantomPinned,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, Sender},
        Arc,
    },
};

use twizzler_abi::syscall::{
    sys_thread_sync, ThreadSync, ThreadSyncFlags, ThreadSyncOp, ThreadSyncReference,
    ThreadSyncSleep, ThreadSyncWake,
};
use twizzler_rt_abi::object::ObjID;

use super::ManagedThread;
use crate::mon::get_monitor;

/// Tracks threads that do not exit cleanly, so their monitor-internal resources can be cleaned up.
pub(crate) struct ThreadCleaner {
    _thread: std::thread::JoinHandle<()>,
    send: Sender<WaitOp>,
    inner: Pin<Arc<ThreadCleanerData>>,
}

#[derive(Default)]
struct ThreadCleanerData {
    notify: AtomicU64,
    _unpin: PhantomPinned,
}

// All the threads we are tracking.
#[derive(Default)]
struct Waits {
    threads: HashMap<ObjID, ManagedThread>,
}

// Changes to the collection of threads we are tracking
enum WaitOp {
    Add(ManagedThread),
    Remove(ObjID),
}

impl ThreadCleaner {
    /// Makes a new ThreadCleaner.
    pub(crate) fn new() -> Self {
        let (send, recv) = std::sync::mpsc::channel();
        let data = Arc::pin(ThreadCleanerData::default());
        let inner = data.clone();
        let thread = std::thread::Builder::new()
            .name("thread-exit cleanup tracker".into())
            .spawn(move || cleaner_thread_main(data, recv))
            .unwrap();
        Self {
            send,
            inner,
            _thread: thread,
        }
    }

    /// Track a thread. If that thread exits, the cleanup thread will remove the exited thread from
    /// tracking and from the global thread manager.
    pub fn track(&self, th: ManagedThread) {
        tracing::debug!("tracking thread {}", th.id);
        let _ = self.send.send(WaitOp::Add(th));
        self.inner.notify();
    }

    /// Untrack a thread. Threads removed this way do not trigger a removal from the global thread
    /// manager.
    pub fn untrack(&self, id: ObjID) {
        let _ = self.send.send(WaitOp::Remove(id));
        self.inner.notify();
    }
}

impl ThreadCleanerData {
    /// Notify the cleanup thread that new items are on the queue.
    fn notify(&self) {
        self.notify.store(1, Ordering::SeqCst);
        let mut ops = [ThreadSync::new_wake(ThreadSyncWake::new(
            ThreadSyncReference::Virtual(&self.notify),
            1,
        ))];
        if let Err(e) = sys_thread_sync(&mut ops, None) {
            tracing::warn!("thread sync error when trying to notify: {}", e);
        }
    }
}

impl Waits {
    fn process_queue(&mut self, recv: &mut Receiver<WaitOp>) {
        while let Ok(wo) = recv.try_recv() {
            match wo {
                WaitOp::Add(th) => {
                    self.threads.insert(th.id, th);
                }
                WaitOp::Remove(id) => {
                    self.threads.remove(&id);
                }
            }
        }
    }
}

fn cleaner_thread_main(data: Pin<Arc<ThreadCleanerData>>, mut recv: Receiver<WaitOp>) {
    // TODO (dbittman): when we have support for async thread events, we can use that API.
    let mut ops = Vec::new();
    let mut cleanups = Vec::new();
    let mut waits = Waits::default();
    let mut key = happylock::ThreadKey::get().unwrap();
    loop {
        ops.truncate(0);
        // Apply any waiting operations.
        waits.process_queue(&mut recv);

        // Add the notify sleep op.
        ops.push(ThreadSync::new_sleep(ThreadSyncSleep::new(
            ThreadSyncReference::Virtual(&data.notify),
            0,
            ThreadSyncOp::Equal,
            ThreadSyncFlags::empty(),
        )));

        // Add all sleep ops for threads.
        cleanups.extend(waits.threads.extract_if(|_, th| th.has_exited()));
        for th in waits.threads.values() {
            ops.push(ThreadSync::new_sleep(th.waitable_until_exit()));
        }

        // Remove any exited threads from the thread manager.
        for (_, th) in cleanups.drain(..) {
            tracing::debug!("cleaning thread: {}", th.id);
            let monitor = get_monitor();
            {
                let mut tmgr = monitor.thread_mgr.write(&mut key);
                tmgr.do_remove(&th);
            }
            let (_, _, ref mut cmgr, ref mut dynlink, _, _) = *monitor.locks.lock(&mut key);
            for comp in cmgr.compartments_mut() {
                comp.clean_per_thread_data(th.id);
            }
            if let Some(comp_id) = th.main_thread_comp {
                cmgr.main_thread_exited(comp_id);
            }
            cmgr.process_cleanup_queue(&mut *dynlink);
        }

        // Check for notifications, and sleep.
        if data.notify.swap(0, Ordering::SeqCst) == 0 {
            // no notification, go to sleep. hold the lock over the sleep so that someone cannot
            // modify waits.threads on us while we're asleep.
            if let Err(e) = sys_thread_sync(&mut ops, None) {
                tracing::warn!("thread sync error: {}", e);
            }
        }
    }
}
