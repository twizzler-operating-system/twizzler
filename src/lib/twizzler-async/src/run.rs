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

/// Runs executors.
///
/// We run both the thread-local executor and the global executor, and also check for timer events.
/// If we cannot make progress, we call the reactor, which handles waiting and waking up on
/// [crate::Async] and [crate::AsyncDuplex] objects for use in externally signaled events that
/// control non-blocking closures' readiness.
///
/// # Examples
/// ```no_run
/// // Run executors on the current thread.
/// run(async {
///     println!("Hello!");
/// });
/// ```
///
/// Multi-threaded:
/// ```no_run
/// use futures::future;
/// let num_threads = 4;
/// for _ in 0..num_threads {
///     // Spawn a pending future.
///     std::thread::spawn(|| twizzler_async::run(future::pending::<()>()))
/// }
///
/// twizzler_async::block_on(async {
///     twizzler_async::Task::spawn(async {
///         println!("Hello from executor thread!");
///     })
///     .await;
/// });
/// ```
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
            react(reactor, &flag_events, more_exec || more_local, true);
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

    if more_tasks {
        reactor.poll(flag_events, try_only);
    } else {
        reactor.wait(flag_events, try_only);
        if !try_only {
            for ev in flag_events {
                ev.clear();
            }
        }
    }
}
