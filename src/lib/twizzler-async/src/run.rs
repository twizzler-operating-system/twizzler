use std::{
    future::Future,
    task::{Context, Poll},
    thread,
};

use crate::{
    event::FlagEvent, exec::Executor, reactor::Reactor, thread_local::ThreadLocalExecutor, throttle,
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

            let more_local = local.execute();
            let more_exec = worker.execute();
            println!("non_block react");
            react(reactor, &flag_events, more_exec || more_local, true);
            println!("non_block react done");
            looks like we're not knowing if we should exectute more???
            let more_local = local.execute();
            let more_exec = worker.execute();
            if more_exec || more_local {
                yields = 0;
                continue;
            }

            yields += 1;
            if yields < 4 {
                thread::yield_now();
                continue;
            }

            yields = 0;

            println!("sleeping");
            react(reactor, &flag_events, false, false);
            println!("woke up");
        }
    })
}

fn react(reactor: &Reactor, flag_events: &[&FlagEvent], mut more_tasks: bool, try_only: bool) {
    for ev in flag_events {
        if ev.clear() {
            more_tasks = true;
        }
    }

    if more_tasks {
        reactor.poll(flag_events, try_only);
    } else {
        reactor.wait(flag_events, try_only);
        for ev in flag_events {
            ev.clear();
        }
    }
}
