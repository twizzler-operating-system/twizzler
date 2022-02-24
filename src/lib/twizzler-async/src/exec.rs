use std::{cell::Cell, collections::VecDeque, future::Future, sync::Mutex};

use futures_util::__private::async_await;
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
                println!("es1 {:?} {:p}", runnable, runnable.tag());
                let mut queue = self.queue.lock().unwrap();
                queue.push_front(runnable);
                println!("es2 {:?} {:p}", queue.as_slices().0[0], &*queue);
                drop(queue);
            }
            self.notify_work();
            println!("es3 {:p}", self);
        };
        println!("spawning");
        let (runnable, handle) = async_task::spawn(future, schedule, 45678);
        println!("spawned {:?} {:p}", runnable, runnable.tag());
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
        println!("x1");
        for _ in 0..4 {
            for _ in 0..50 {
                println!("x2");
                match self.search() {
                    None => {
                        println!("x2n");
                        return false;
                    }
                    Some(r) => {
                        println!("x3");
                        // TODO: why?
                        self.exec.notify_work();

                        println!("x4");
                        if throttle::setup(|| {
                            println!("x4r {:?} {:p}", r, r.tag());
                            let x = r.run();
                            println!("x4r2");
                            x
                        }) {
                            //println!("x4t");
                            //self.flush_current();
                        }
                        println!("x5");
                    }
                }
            }
            println!("x6");
            //self.flush_current();
            //println!("x7");
        }
        true
    }

    /*
    fn flush_current(&self) {
        println!("f0");
        if let Some(r) = self.current.take() {
            println!("f1");
            self.exec.queue.lock().unwrap().push_back(r);
            println!("f2");
        }
        println!("f3");
    }
    */

    #[allow(named_asm_labels)]
    fn search(&self) -> Option<Runnable> {
        println!("s1");
        //if let Some(r) = self.current.take() {
        //    println!("s1r");
        //     return Some(r);
        // }
        {
            println!("s2 {} {:p}", self.exec.queue.try_lock().is_ok(), self.exec);
        }
        let mut queue = self.exec.queue.lock().unwrap();
        queue.pop_front()
    }
}

impl Drop for Worker<'_> {
    fn drop(&mut self) {
        println!("dropping worker");
        // if let Some(r) = self.current.take() {
        //      r.schedule();
        // }
        self.exec.notify_work();
    }
}
