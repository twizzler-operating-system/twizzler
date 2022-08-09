use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Poll, Waker},
    time::{Duration, Instant},
};

use stable_vec::StableVec;
use twizzler_abi::syscall::{ThreadSync, ThreadSyncSleep};

use crate::event::FlagEvent;

lazy_static::lazy_static! {
    static ref REACTOR: Reactor = {
        Reactor {
            sources: Mutex::new(StableVec::new()),
            timers: Mutex::new(BTreeMap::new()),
            timer_ops: Mutex::new(VecDeque::new()),
            timer_event: FlagEvent::new(),
        }
    };
}

enum TimerOp {
    Insert(Instant, usize, Waker),
    Remove(Instant, usize),
}

pub(crate) struct Reactor {
    sources: Mutex<StableVec<Arc<Source>>>,
    timers: Mutex<BTreeMap<(Instant, usize), Waker>>,
    timer_ops: Mutex<VecDeque<TimerOp>>,
    timer_event: FlagEvent,
}

impl Reactor {
    pub fn get() -> &'static Reactor {
        &REACTOR
    }

    pub fn insert_wait_op(&self, op: ThreadSyncSleep) -> Arc<Source> {
        let mut sources = self.sources.lock().unwrap();
        let index = sources
            .first_empty_slot_from(0)
            .unwrap_or_else(|| sources.next_push_index());
        let source = Arc::new(Source::new(op, index));
        sources.reserve_for(index);
        let old = sources.insert(index, source.clone());
        assert!(old.is_none());
        // TODO: is this necessary?
        self.timer_event.notify();
        source
    }

    pub fn remove_wait_op(&self, source: &Source) {
        let mut sources = self.sources.lock().unwrap();
        let res = sources.remove(source.key);
        assert!(res.is_some());
    }

    pub fn insert_timer(&self, when: Instant, waker: &Waker) -> usize {
        static ID_GEN: AtomicUsize = AtomicUsize::new(1);
        let id = ID_GEN.fetch_add(1, Ordering::SeqCst);

        self.timer_ops
            .lock()
            .unwrap()
            .push_back(TimerOp::Insert(when, id, waker.clone()));
        self.timer_event.notify();
        id
    }

    pub fn remove_timer(&self, when: Instant, id: usize) {
        self.timer_ops
            .lock()
            .unwrap()
            .push_back(TimerOp::Remove(when, id));
    }

    pub fn fire_timers(&self) -> Option<Duration> {
        self.timer_event.clear();
        let (ready, dur) = {
            let mut timers = self.timers.lock().unwrap();
            {
                let mut timer_ops = self.timer_ops.lock().unwrap();
                while let Some(op) = timer_ops.pop_front() {
                    match op {
                        TimerOp::Insert(when, id, waker) => {
                            timers.insert((when, id), waker);
                        }
                        TimerOp::Remove(when, id) => {
                            timers.remove(&(when, id));
                        }
                    }
                }
                drop(timer_ops);
            }

            let now = Instant::now();
            let pending = timers.split_off(&(now, 0));
            let ready = core::mem::replace(&mut *timers, pending);

            let dur = if ready.is_empty() {
                timers
                    .keys()
                    .next()
                    .map(|(when, _)| when.saturating_duration_since(now))
            } else {
                Some(Duration::from_secs(0))
            };
            drop(timers);
            (ready, dur)
        };
        for (_, waker) in ready {
            waker.wake();
        }

        dur
    }

    pub fn poll(&self, flag_events: &[&FlagEvent], try_only: bool) {
        self.react(flag_events, false, try_only);
    }

    pub fn wait(&self, flag_events: &[&FlagEvent], try_only: bool) {
        self.react(flag_events, true, try_only);
    }

    // TODO: one major optimization we could make is to keep separate lists of active sources.
    fn react(&self, flag_events: &[&FlagEvent], block: bool, try_only: bool) -> Option<()> {
        let next_timer = self.fire_timers();
        let timeout = if block {
            next_timer
        } else {
            Some(Duration::from_secs(0))
        };
        let sources = if try_only {
            self.sources.try_lock().ok()?
        } else {
            self.sources.lock().unwrap()
        };
        let mut events = vec![];
        for (_, src) in &*sources {
            let inner = src.inner.lock().unwrap();
            if !inner.active {
                continue;
            }
            if inner.op.ready() {
                inner.wake_all();
                return None;
            }
            events.push(ThreadSync::new_sleep(inner.op));
        }

        if !block || try_only {
            return None;
        }

        for fe in flag_events {
            let s = fe.setup_sleep();
            if s.ready() {
                return None;
            }
            events.push(ThreadSync::new_sleep(s));
        }

        let s = self.timer_event.setup_sleep();
        if s.ready() {
            return None;
        }
        events.push(ThreadSync::new_sleep(s));

        drop(sources);
        // TODO: check err
        if timeout != Some(Duration::from_nanos(0)) {
            let _ = twizzler_abi::syscall::sys_thread_sync(events.as_mut_slice(), timeout);
        }

        let sources = self.sources.lock().unwrap();
        for (_, src) in &*sources {
            let inner = src.inner.lock().unwrap();
            if inner.op.ready() {
                inner.wake_all();
            }
        }
        self.fire_timers();
        Some(())
    }
}

#[derive(Debug)]
struct SourceInner {
    op: ThreadSyncSleep,
    wakers: Vec<Waker>,
    active: bool,
}

#[derive(Debug)]
pub(crate) struct Source {
    key: usize,
    inner: Mutex<SourceInner>,
}

unsafe impl Send for Source {}
unsafe impl Send for SourceInner {}
unsafe impl Sync for Source {}
unsafe impl Sync for SourceInner {}

impl SourceInner {
    fn wake_all(&self) {
        for w in &self.wakers {
            w.wake_by_ref();
        }
    }
}

impl Source {
    fn new(op: ThreadSyncSleep, key: usize) -> Self {
        Self {
            inner: Mutex::new(SourceInner {
                op,
                wakers: vec![],
                active: false,
            }),
            key,
        }
    }

    pub(crate) async fn runnable(&self, sleep_op: ThreadSyncSleep) {
        let mut polled = false;
        // TODO: this mutex shouldn't be necessary
        let tmp = Mutex::new(sleep_op);
        futures_util::future::poll_fn(|cx| {
            let mut inner = self.inner.lock().unwrap();
            if polled {
                inner.active = false;
                Poll::Ready(())
            } else {
                inner.active = true;
                inner.op = *tmp.lock().unwrap();
                if inner.wakers.iter().all(|w| !w.will_wake(cx.waker())) {
                    inner.wakers.push(cx.waker().clone());
                }
                Reactor::get().timer_event.notify();

                polled = true;
                Poll::Pending
            }
        })
        .await
    }
}
