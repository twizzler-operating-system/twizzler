use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use crate::thread_local::ThreadLocalExecutor;

pub(crate) type Runnable = async_task::Task<u32>;

/// A spawned future. Tasks are futures themselves and yield the output of the spawned future.
///
/// When a task is dropped, it is automatically canceled and it won't be polled again. You can also
/// cancel a task explicitly with the [`cancel()`][Task::cancel()] method.
///
/// Tasks that panic are immediately canceled, and awaiting a canceled task causes a panic. If the
/// future panics, the panic will be unwound into the [`run()`][crate::run()] invocation that polled it, but this
/// doesn't apply to the blocking executor, which will simply ignore panics and continue running.
#[must_use = "futures do nothing unless you `.await` or poll them; tasks, specifically, get canceled if you drop them, use `.detach()` to run them in the background"]
pub struct Task<T>(pub(crate) Option<async_task::JoinHandle<T, u32>>);

impl<T: 'static> Task<T> {
    /// Spawns a future onto the thread-local executor.
    ///
    /// Panics if the current thread is not inside an invocation of [`run()`][crate::run()].
    pub fn local(future: impl Future<Output = T> + 'static) -> Task<T> {
        ThreadLocalExecutor::spawn(future)
    }
}

impl<T: Send + 'static> Task<T> {
    /// Spawns a future onto the global executor.
    ///
    /// This future may be stolen and polled by any thread calling [`run()`][crate::run()], and thus the future
    /// (and its output) must be Send.
    pub fn spawn(future: impl Future<Output = T> + Send + 'static) -> Task<T> {
        crate::exec::Executor::get().spawn(future)
    }
}

impl<T, E> Task<Result<T, E>>
where
    T: Send + 'static,
    E: std::fmt::Debug + Send + 'static,
{
    /// Spawns a new task and awaits and unwraps the result.
    pub fn unwrap(self) -> Task<T> {
        Task::spawn(async { self.await.unwrap() })
    }

    /// Spawns a new task and awaits and unwraps the result, panicing with the provided message if
    /// the unwrap fails.
    pub fn expect(self, msg: &str) -> Task<T> {
        let msg = msg.to_owned();
        Task::spawn(async move { self.await.expect(&msg) })
    }
}

impl Task<()> {
    /// Detach the task and let it run in the background.
    /// # Examples
    ///
    /// ```no_run
    /// use twizzler_async::{Task, Timer};
    /// use std::time::Duration;
    ///
    /// # twizzler_async::run(async {
    /// Task::spawn(async {
    ///     loop {
    ///         println!("I'm a daemon task looping forever.");
    ///         Timer::after(Duration::from_secs(1)).await;
    ///     }
    /// })
    /// .detach();
    /// # })
    /// ```
    pub fn detach(mut self) {
        self.0.take().unwrap();
    }
}

impl<T> Task<T> {
    /// Cancels the task and waits for it to stop running. If the task completed before canceling,
    /// return the task's output, or `None` if it wasn't complete. The advantage of calling
    /// `cancel()` explicitly over jus dropping the task is that it, one, waits for the task to stop
    /// running before returning, and two, it returns the result if the task _did_ successfully complete.
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
