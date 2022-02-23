use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

pub(crate) type Runnable = async_task::Task<u32>;

pub struct Task<T>(pub(crate) Option<async_task::JoinHandle<T, u32>>);

impl<T: Send + 'static> Task<T> {
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        crate::exec::Executor::get().spawn(future)
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
{
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
    }

    pub fn expect(self, msg: &str) -> Task<T> {
        let msg = msg.to_owned();
        Task::spawn(async move { self.await.expect(&msg) })
    }
}

impl Task<()> {
    pub fn detach(mut self) {
        self.0.take().unwrap();
    }
}

impl<T> Task<T> {
    pub async fn cancel(self) -> Option<T> {
        let handle = { self }.0.take().unwrap();
        handle.cancel();
        handle.await
    }
}

impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        if let Some(handle) = &self.0 {
            handle.cancel()
        }
    }
}

impl<T> Future for Task<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0.as_mut().unwrap()).poll(cx) {
            Poll::Ready(output) => Poll::Ready(output.expect("task failed")),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<T> Into<async_task::JoinHandle<T, u32>> for Task<T> {
    fn into(mut self) -> async_task::JoinHandle<T, u32> {
        self.0.take().expect("task was already canceled or failed")
    }
}
