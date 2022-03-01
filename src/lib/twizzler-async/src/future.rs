use std::{
    future::Future,
    sync::{Arc, Mutex},
    task::{Poll, Waker},
};

use futures_util::FutureExt;

pub struct WaitForFirst<FutOne, FutTwo> {
    one: FutOne,
    two: Option<FutTwo>,
}

impl<FutOne: Unpin, FutTwo: Unpin> Unpin for WaitForFirst<FutOne, FutTwo> {}

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
            if let Poll::Ready(_) = two.poll_unpin(cx) {
                self.two = None;
            }
        }

        Poll::Pending
    }
}

pub struct FlagBlockInner {
    wakers: Vec<Waker>,
    epoch: u64,
}

pub struct FlagBlock {
    inner: Arc<Mutex<FlagBlockInner>>,
}

pub struct FlagBlockFuture<'a> {
    state: &'a FlagBlock,
    val: u64,
    added: bool,
}

impl FlagBlock {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FlagBlockInner {
                wakers: vec![],
                epoch: 0,
            })),
        }
    }

    pub fn signal_all(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.epoch += 1;
        while let Some(w) = inner.wakers.pop() {
            w.wake();
        }
    }

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
