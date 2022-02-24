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
                println!("-1 done");
                return val;
            }

            println!("0");
            let more_local = local.execute();
            println!("1");
            let more_exec = worker.execute();
            println!("2");
            react(reactor, &flag_events, more_exec || more_local, true);
            println!("3");
            if more_exec || more_local {
                yields = 0;
                println!("3c");
                continue;
            }

            yields += 1;
            if yields < 4 {
                println!("3cc");
                thread::yield_now();
                continue;
            }

            yields = 0;

            println!("4");
            react(reactor, &flag_events, false, false);
        }
    })
}

fn react(reactor: &Reactor, flag_events: &[&FlagEvent], mut more_tasks: bool, try_only: bool) {
    for ev in flag_events {
        if ev.clear() {
            more_tasks = true;
        }
    }

    println!("react {}", more_tasks);
    if more_tasks {
        reactor.poll(flag_events, try_only);
    } else {
        reactor.wait(flag_events, try_only);
        for ev in flag_events {
            ev.clear();
        }
    }
}
