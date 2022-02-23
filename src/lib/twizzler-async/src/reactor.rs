use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, MutexGuard, Once},
    task::Waker,
};

use stable_vec::StableVec;
use twizzler_abi::syscall::{ThreadSync, ThreadSyncSleep};

use crate::event::{ExtEvent, FlagEvent};

lazy_static::lazy_static! {
    static ref REACTOR: Reactor = {
        Reactor {
            sources: Mutex::new(StableVec::new()),
        }
    };
}

pub(crate) struct Reactor {
    sources: Mutex<StableVec<Arc<Source>>>,
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
        let old = sources.insert(index, source.clone());
        assert!(old.is_none());
        source
    }

    pub fn remove_wait_op(&self, source: &Source) {
        let mut sources = self.sources.lock().unwrap();
        let res = sources.remove(source.key);
        assert!(res.is_some());
    }

    pub fn lock(&self) -> ReactorLock<'_> {
        ReactorLock {
            reactor: self,
            sources_guard: self.sources.lock().unwrap(),
        }
    }

    pub fn try_lock(&self) -> Option<ReactorLock<'_>> {
        Some(ReactorLock {
            reactor: self,
            sources_guard: self.sources.try_lock().ok()?,
        })
    }
}

pub(crate) struct ReactorLock<'a> {
    reactor: &'a Reactor,
    sources_guard: MutexGuard<'a, StableVec<Arc<Source>>>,
}

impl ReactorLock<'_> {
    pub fn poll(&mut self, flag_events: &[&FlagEvent]) {
        self.react(flag_events, false);
    }

    pub fn wait(&mut self, flag_events: &[&FlagEvent]) {
        self.react(flag_events, true)
    }

    fn react(&mut self, flag_events: &[&FlagEvent], block: bool) {
        if !block {
            return;
        }
        let mut events = vec![];
        for (_, src) in &*self.sources_guard {
            events.push(ThreadSync::new_sleep(src.op));
        }

        for fe in flag_events {
            events.push(ThreadSync::new_sleep(fe.setup_sleep()));
        }

        for ev in &events {
            if ev.ready() {
                return;
            }
        }

        // TODO: check err
        let _ = twizzler_abi::syscall::sys_thread_sync(events.as_mut_slice(), None);
    }
}

pub(crate) struct Source {
    op: ThreadSyncSleep,
    wakers: Mutex<Vec<Waker>>,
    key: usize,
}
unsafe impl Send for Source {}
unsafe impl Sync for Source {}

impl Source {
    fn new(op: ThreadSyncSleep, key: usize) -> Self {
        Self {
            op,
            wakers: Mutex::new(vec![]),
            key,
        }
    }
}
