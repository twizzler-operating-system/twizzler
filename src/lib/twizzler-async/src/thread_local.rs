use std::{
    cell::RefCell,
    collections::VecDeque,
    sync::{Arc, Mutex},
};

use std::thread::{self, ThreadId};

use scoped_tls_hkt::scoped_thread_local;
use std::future::Future;

use crate::{
    event::FlagEvent,
    task::{Runnable, Task},
    throttle,
};

scoped_thread_local! {
    static EXECUTOR: ThreadLocalExecutor
}

pub(crate) struct ThreadLocalExecutor {
    queue: RefCell<VecDeque<Runnable>>,
    injector: Arc<Mutex<VecDeque<Runnable>>>,
    avail: FlagEvent,
}

impl ThreadLocalExecutor {
    pub fn new() -> ThreadLocalExecutor {
        ThreadLocalExecutor {
            queue: RefCell::new(VecDeque::new()),
            injector: Arc::new(Mutex::new(VecDeque::new())),
            avail: FlagEvent::new(),
        }
    }

    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        if EXECUTOR.is_set() {
            panic!("cannot run executors recursively");
        }
        EXECUTOR.set(self, f)
    }

    pub fn event(&self) -> &FlagEvent {
        &self.avail
    }

    pub fn spawn<T: 'static>(future: impl Future<Output = T> + 'static) -> Task<T> {
        if !EXECUTOR.is_set() {
            panic!("cannot spawn a thread-local task if not inside an executor");
        }

        EXECUTOR.with(|ex| {
            let injector = Arc::downgrade(&ex.injector);
            let event = ex.event().clone();
            let id = thread_id();
            let schedule = move |runnable| {
                if thread_id() == id {
                    EXECUTOR.with(|ex| ex.queue.borrow_mut().push_back(runnable));
                } else if let Some(injector) = injector.upgrade() {
                    injector.lock().unwrap().push_back(runnable);
                }
                event.notify();
            };

            let (runnable, handle) = async_task::spawn_local(future, schedule, 12345);
            runnable.schedule();
            Task(Some(handle))
        })
    }

    pub fn execute(&self) -> bool {
        for _ in 0..4 {
            for _ in 0..50 {
                match self.search() {
                    Some(r) => {
                        throttle::setup(|| r.run());
                    }
                    None => return false,
                }
            }
            self.fetch();
        }
        true
    }

    fn search(&self) -> Option<Runnable> {
        if let Some(r) = self.queue.borrow_mut().pop_front() {
            return Some(r);
        }
        self.fetch();
        self.queue.borrow_mut().pop_front()
    }

    fn fetch(&self) {
        let mut queue = self.queue.borrow_mut();
        let mut injector = self.injector.lock().unwrap();
        while let Some(r) = injector.pop_front() {
            queue.push_back(r);
        }
    }
}

fn thread_id() -> ThreadId {
    thread_local! {
        static ID: ThreadId = thread::current().id();
    }

    ID.try_with(|id| *id)
        .unwrap_or_else(|_| thread::current().id())
}
