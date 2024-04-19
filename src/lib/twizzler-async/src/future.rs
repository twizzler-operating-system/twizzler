use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
    time::{Duration, Instant},
};

use futures_util::FutureExt;

use crate::Timer;

/// A future that waits on two sub-futures until the first completes.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct WaitForFirst<FutOne, FutTwo> {
    one: FutOne,
    two: Option<FutTwo>,
}

impl<FutOne: Unpin, FutTwo: Unpin> Unpin for WaitForFirst<FutOne, FutTwo> {}

/// A future that waits on two sub-futures until the first one completes. If the second one
/// completes first, this future will continue awaiting on the first future. If the first one
/// completes first, this future returns immediately without continuing to wait on the second
/// future.
pub fn wait_for_first<FutOne, FutTwo, T, R>(
    one: FutOne,
    two: FutTwo,
) -> WaitForFirst<FutOne, FutTwo>
where
    FutOne: Future<Output = T>,
    FutTwo: Future<Output = R>,
{
    WaitForFirst {
        one,
        two: Some(two),
    }
}

impl<FutOne, FutTwo> WaitForFirst<FutOne, FutTwo> {
    pub fn into_inner(self) -> (FutOne, Option<FutTwo>) {
        (self.one, self.two)
    }
}

impl<FutOne: Future + Unpin, FutTwo: Future + Unpin> Future for WaitForFirst<FutOne, FutTwo> {
    type Output = FutOne::Output;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if let Poll::Ready(e) = self.one.poll_unpin(cx) {
            return Poll::Ready(e);
        }

        if let Some(two) = &mut self.two {
            if two.poll_unpin(cx).is_ready() {
                self.two = None;
            }
        }

        Poll::Pending
    }
}

#[derive(Default)]
pub struct FlagBlockInner {
    wakers: Vec<Waker>,
    epoch: u64,
}

#[derive(Default)]
/// A basic condition variable for async tasks. If you call wait() you get back a future that you
/// can await on, which will complete once another tasks calls signal_all(). But there's a gotcha
/// here.
///
/// Okay so you know the rule with mutexes and condition variables? Like, you have some predicate
/// that tells you "ready" or not, and this is tested with the mutex held, followed by waiting on
/// the condition variable (which automatically releases and reaquires the lock).
///
/// We have something similar. You need to call wait() with that mutex held, and then _after_ you
/// release the lock, you call await on the future returned by wait().
pub struct FlagBlock {
    inner: Arc<Mutex<FlagBlockInner>>,
}

#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct FlagBlockFuture<'a> {
    state: &'a FlagBlock,
    val: u64,
    added: bool,
}

impl FlagBlock {
    /// Construct a new FlagBlock.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FlagBlockInner {
                wakers: vec![],
                epoch: 0,
            })),
        }
    }

    /// Signal anyone waiting.
    pub fn signal_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.epoch += 1;
        while let Some(w) = inner.wakers.pop() {
            w.wake();
        }
    }

    /// Return an awaitable future for the "readiness" of this condition variable.
    pub fn wait(&self) -> FlagBlockFuture {
        let inner = self.inner.lock().unwrap();
        FlagBlockFuture {
            state: self,
            added: false,
            val: inner.epoch,
        }
    }
}

impl<'a> Future for FlagBlockFuture<'a> {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        let mut inner = self.state.inner.lock().unwrap();
        if inner.epoch != self.val {
            Poll::Ready(())
        } else {
            if !self.added {
                inner.wakers.push(cx.waker().clone());
                drop(inner);
                self.added = true;
            }
            Poll::Pending
        }
    }
}

/// A future that awaits a sub-future until a timeout occurs.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Timeout<T> {
    value: T,
    delay: Timer,
}

impl<T> Timeout<T> {
    pub fn after(f: T, dur: Duration) -> Self {
        Self {
            value: f,
            delay: Timer::after(dur),
        }
    }

    pub fn at(f: T, at: Instant) -> Self {
        Self {
            value: f,
            delay: Timer::at(at),
        }
    }
}

impl<T: Future + Unpin> Future for Timeout<T> {
    type Output = Option<T::Output>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        match self.value.poll_unpin(cx) {
            Poll::Ready(res) => return Poll::Ready(Some(res)),
            Poll::Pending => {}
        }

        match self.delay.poll_unpin(cx) {
            Poll::Ready(_) => return Poll::Ready(None),
            Poll::Pending => {}
        }
        Poll::Pending
    }
}

/// Await a future until a timeout occurs (or that future completes). If the timeout happens, return
/// None, otherwise return Some of the result of the future. This timeout expires after a duration.
pub async fn timeout_after<F: Future>(f: F, dur: Duration) -> Option<F::Output> {
    Timeout::after(Box::pin(f), dur).await
}

/// Await a future until a timeout occurs (or that future completes). If the timeout happens, return
/// None, otherwise return Some of the result of the future. This timeout expires at an instant in
/// time.
pub async fn timeout_at<F: Future>(f: F, at: Instant) -> Option<F::Output> {
    Timeout::at(Box::pin(f), at).await
}
