use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    task::{Context, Poll},
    time::Duration,
};

use std::cell::RefCell;
use std::future::Future;
use std::task::Waker;
struct Parker {
    unparker: Unparker,
}
const EMPTY: usize = 0;
const PARKED: usize = 1;
const NOTIFIED: usize = 2;

struct Inner {
    state: AtomicUsize,
    lock: Mutex<()>,
    cvar: Condvar,
}

impl Parker {
    fn new() -> Self {
        Self {
            unparker: Unparker {
                inner: Arc::new(Inner {
                    state: AtomicUsize::new(EMPTY),
                    lock: Mutex::new(()),
                    cvar: Condvar::new(),
                }),
            },
        }
    }

    fn park(&self) {
        self.unparker.inner.park(None);
    }

    //pub fn park_timeout(&self, timeout: Duration) {
    //    self.unparker.inner.park(Some(timeout));
    //}

    //pub fn unparker(&self) -> &Unparker {
    //    &self.unparker
    //}
}

struct Unparker {
    inner: Arc<Inner>,
}

unsafe impl Send for Parker {}

impl Unparker {
    pub fn unpark(&self) {
        self.inner.unpark()
    }
}
unsafe impl Send for Unparker {}
unsafe impl Sync for Unparker {}

impl Clone for Unparker {
    fn clone(&self) -> Unparker {
        Unparker {
            inner: self.inner.clone(),
        }
    }
}

impl Inner {
    fn park(&self, timeout: Option<Duration>) {
        if self
            .state
            .compare_exchange(NOTIFIED, EMPTY, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            return;
        }

        if let Some(ref dur) = timeout {
            if *dur == Duration::from_millis(0) {
                return;
            }
        }

        let mut m = self.lock.lock().unwrap();
        match self
            .state
            .compare_exchange(EMPTY, PARKED, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {}
            Err(NOTIFIED) => {
                let _old = self.state.swap(EMPTY, Ordering::SeqCst);
                return;
            }
            Err(_) => panic!("invalid park state"),
        }

        match timeout {
            None => loop {
                m = self.cvar.wait(m).unwrap();
                if self
                    .state
                    .compare_exchange(NOTIFIED, EMPTY, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    return;
                }
            },
            Some(timeout) => {
                let (_m, _result) = self.cvar.wait_timeout(m, timeout).unwrap();
                match self.state.swap(EMPTY, Ordering::SeqCst) {
                    NOTIFIED => {}
                    PARKED => {}
                    n => panic!("invalid park state {}", n),
                }
            }
        }
    }

    fn unpark(&self) {
        match self.state.swap(NOTIFIED, Ordering::SeqCst) {
            EMPTY => return,
            NOTIFIED => return,
            PARKED => {}
            _ => panic!("invalid park state"),
        }

        drop(self.lock.lock().unwrap());
        self.cvar.notify_one();
    }
}

/// Run a future to completion, sleeping the thread if there is no progress that can be made.
pub fn block_on<T>(future: impl Future<Output = T>) -> T {
    thread_local! {
        static CACHE: RefCell<(Parker, Waker)> = {
            let parker = Parker::new();
            let unparker = parker.unparker.clone();
            let waker = async_task::waker_fn(move || unparker.unpark());
            RefCell::new((parker, waker))
        };
    }

    CACHE.with(|cache| {
        let (parker, waker) = &mut *cache.try_borrow_mut().expect("recursive block_on");
        crate::run::enter(|| {
            futures_util::pin_mut!(future);
            let cx = &mut Context::from_waker(waker);
            loop {
                match future.as_mut().poll(cx) {
                    Poll::Ready(output) => return output,
                    Poll::Pending => parker.park(),
                }
            }
        })
    })
}
