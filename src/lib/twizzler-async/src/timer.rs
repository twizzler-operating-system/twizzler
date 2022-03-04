use std::{
    future::Future,
    task::Poll,
    time::{Duration, Instant},
};

/// A timer future that returns after a specified period of time.
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Timer {
    id: Option<usize>,
    when: Instant,
}

impl Timer {
    /// Make a new timer future that returns Ready after a specified duration.
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

    /// Make a new timer future that returns Ready at or after a specified instant in time.
    pub fn at(when: Instant) -> Timer {
        Timer { id: None, when }
    }
}

impl Drop for Timer {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            crate::reactor::Reactor::get().remove_timer(self.when, id);
        }
    }
}

impl Future for Timer {
    type Output = Instant;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        if Instant::now() >= self.when {
            if let Some(id) = self.id.take() {
                crate::reactor::Reactor::get().remove_timer(self.when, id);
            }
            Poll::Ready(self.when)
        } else {
            if self.id.is_none() {
                self.id = Some(crate::reactor::Reactor::get().insert_timer(self.when, cx.waker()));
            }
            Poll::Pending
        }
    }
}
