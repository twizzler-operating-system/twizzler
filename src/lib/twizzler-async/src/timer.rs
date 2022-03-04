use std::{
    future::Future,
    task::Poll,
    time::{Duration, Instant},
};

#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Timer {
    id: Option<usize>,
    when: Instant,
}

impl Timer {
    pub fn after(dur: Duration) -> Timer {
        Timer::at(Instant::now() + dur)
    }

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
