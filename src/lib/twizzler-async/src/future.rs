use std::{future::Future, task::Poll};

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
