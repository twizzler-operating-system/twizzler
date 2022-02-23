use std::{
    future::Future,
    task::{Context, Poll},
    thread,
};

use futures_util::future::Either;

use crate::{
    event::FlagEvent,
    exec::Executor,
    reactor::{Reactor, ReactorLock},
    thread_local::ThreadLocalExecutor,
    throttle,
};

pub(crate) fn enter<T>(f: impl FnOnce() -> T) -> T {
    f()
}

pub fn run<T>(future: impl Future<Output = T>) -> T {
    let local = ThreadLocalExecutor::new();
    let exec = Executor::get();
    let worker = exec.worker();
    let reactor = Reactor::get();
    let ev = local.event().clone();
    let waker = async_task::waker_fn(move || ev.notify());
    let cx = &mut Context::from_waker(&waker);
    futures_util::pin_mut!(future);

    let enter = |f| local.enter(|| enter(f));
    let enter = |f| worker.enter(|| enter(f));

    enter(|| {
        let mut yields = 0;
        let flag_events = [local.event(), exec.event()];
        loop {
            if let Poll::Ready(val) = throttle::setup(|| future.as_mut().poll(cx)) {
                return val;
            }

            let more = worker.execute();

            if let Some(reactor_lock) = reactor.try_lock() {
                yields = 0;
                react(reactor_lock, &flag_events, more);
                continue;
            }

            if more {
                yields = 0;
                continue;
            }

            yields += 1;
            if yields < 4 {
                thread::yield_now();
                continue;
            }

            yields = 0;

            let lock = reactor.lock();
            react(lock, &flag_events, false)
        }
    });
    todo!()
}

fn react(mut reactor_lock: ReactorLock<'_>, flag_events: &[&FlagEvent], mut more_tasks: bool) {
    for ev in flag_events {
        if ev.clear() {
            more_tasks = true;
        }
    }

    if more_tasks {
        reactor_lock.poll(flag_events);
    } else {
        reactor_lock.wait(flag_events);
        for ev in flag_events {
            ev.clear();
        }
    }
}
