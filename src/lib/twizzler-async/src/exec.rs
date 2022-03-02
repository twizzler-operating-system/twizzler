use std::{collections::VecDeque, future::Future, sync::Mutex};

use scoped_tls_hkt::scoped_thread_local;

use crate::{
    event::FlagEvent,
    task::{Runnable, Task},
    throttle,
};

scoped_thread_local! {
    static WORKER: for<'a> &'a Worker<'a>
}

pub(crate) struct Executor {
    avail: FlagEvent,
    queue: Mutex<VecDeque<Runnable>>,
}

lazy_static::lazy_static! {
    static ref EXECUTOR: Executor = {
        Executor {
            avail: FlagEvent::new(),
            queue: Mutex::new(VecDeque::new()),
        }
    };
}

impl Executor {
    pub fn get() -> &'static Self {
        &EXECUTOR
    }

    pub fn notify_work(&self) {
        self.event().notify();
    }

    pub fn event(&self) -> &FlagEvent {
        &self.avail
    }

    pub fn spawn<T: Send + 'static>(
        &'static self,
        future: impl Future<Output = T> + Send + 'static,
    ) -> Task<T> {
        let schedule = move |runnable: async_task::Task<u32>| {
            {
                let mut queue = self.queue.lock().unwrap();
                queue.push_front(runnable);
                drop(queue);
            }
            self.notify_work();
        };
        let (runnable, handle) = async_task::spawn(future, schedule, 45678);
        runnable.schedule();
        Task(Some(handle))
    }

    pub fn worker(&self) -> Worker<'_> {
        Worker {
            // current: Cell::new(None),
            exec: self,
        }
    }
}

pub(crate) struct Worker<'a> {
    //current: Cell<Option<Runnable>>,
    exec: &'a Executor,
}

impl Worker<'_> {
    pub fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        if WORKER.is_set() {
            panic!("cannot run an executor recursively");
        }
        WORKER.set(self, f)
    }

    pub fn execute(&self) -> bool {
        for _ in 0..4 {
            for _ in 0..50 {
                match self.search() {
                    None => {
                        return false;
                    }
                    Some(r) => {
                        // TODO: why?
                        self.exec.notify_work();

                        if throttle::setup(|| r.run()) {}
                    }
                }
            }
        }
        true
    }

    #[allow(named_asm_labels)]
    fn search(&self) -> Option<Runnable> {
        let mut queue = self.exec.queue.lock().unwrap();
        queue.pop_front()
    }
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        self.exec.notify_work();
    }
}
